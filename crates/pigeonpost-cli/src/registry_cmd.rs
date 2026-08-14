//! `pigeonpost registry serve` — run the bounded, supervised handle registry.
//!
//! Read-only service remains available without identity-provider credentials. Once any provider is
//! enabled, startup requires witnessed custody keys and purpose-separated sealed claim traces.

use std::collections::HashSet;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use fs2::FileExt;
use pigeonpost_compliance_format::{
    validate_compliance_epoch, ComplianceKeyId, CompliancePurpose, Jurisdiction,
    COMPLIANCE_KEY_ID_LEN,
};
use pigeonpost_compliance_seal::{
    required_identity_trace_storage_bytes, required_network_trace_storage_bytes,
};
use pigeonpost_core::{keys, Identity};
use pigeonpost_loft::{
    AttributionKeyResolver, ResolvedTraceKey, TraceKeyResolver, WitnessedRegistryConfig,
    WitnessedRegistryKeyCache,
};
use pigeonpost_registry::{
    claim_trace::{
        ClaimTraceError, ClaimTraceKeyResolver, ClaimTraceSink, ResolvedClaimTraceKey,
        SealedClaimTraceConfig, SealedClaimTraceSink,
    },
    entry::{ComplianceKeyPublish, ComplianceKeyStatus, LogEntry},
    identity::{GithubProvider, GoogleProvider},
    serve_loopback_read_only, serve_witnessed, ComplianceKeyQuery, RegistrationLimits, Registry,
    RegistryConfig, RegistryError, RegistryHttpConfig, RegistryLimits, WitnessSupervisor,
    MAX_REGISTRATION_BINDINGS_PER_MINUTE, MAX_REGISTRY_BLOCKING_OPERATIONS,
    MAX_REGISTRY_BLOCKING_TIMEOUT_MS, MAX_REGISTRY_CONCURRENT_CONNECTIONS,
    MAX_REGISTRY_CONCURRENT_REQUESTS, MAX_REGISTRY_DUMP_STREAMS,
    MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE, MAX_REGISTRY_HEADER_TIMEOUT_MS,
    MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE, MAX_REGISTRY_SOURCE_KEYS,
    MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE,
};
use serde::Deserialize;
use zeroize::Zeroizing;

use pigeonpost_directory::private_store::{
    read_trusted_file_bounded, PrivateDirectory, PrivateFile,
};

#[cfg(feature = "test-utilities")]
use pigeonpost_registry::serve_loopback_test_supervised;

use crate::runtime_config::{
    load_optional_toml, read_existing_seed, read_owner_only_secret, resolve_path,
    resolve_trace_storage_gib, validate_segment_limit, validate_separate_directories,
    validate_trace_storage_requirement, RegistryWitnessingFile, ResolvedRegistryWitnessing,
    TracePolicyFile, WitnessedRegistryFile,
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    server: RegistryServerFile,
    witnessing: Option<RegistryWitnessingFile>,
    compliance: Option<RegistryComplianceFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryServerFile {
    #[serde(default)]
    trusted_proxies: Vec<IpAddr>,
    #[serde(default)]
    directory_publisher_keys: Vec<String>,
    #[serde(default)]
    limits: RegistryLimitsFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryLimitsFile {
    max_concurrent_connections: Option<usize>,
    max_concurrent_requests: Option<usize>,
    max_blocking_operations: Option<usize>,
    max_dump_streams: Option<usize>,
    blocking_timeout_ms: Option<u64>,
    header_timeout_ms: Option<u64>,
    global_requests_per_minute: Option<u32>,
    global_response_bytes_per_minute: Option<u64>,
    source_challenges_per_minute: Option<u32>,
    source_bindings_per_minute: Option<u32>,
    max_source_keys: Option<usize>,
    account_bindings_per_minute: Option<u32>,
    max_account_keys: Option<usize>,
}

impl RegistryLimitsFile {
    fn resolve(&self) -> Result<(RegistryLimits, RegistrationLimits), Box<dyn std::error::Error>> {
        let defaults = RegistryLimits::default();
        let limits = RegistryLimits {
            max_concurrent_connections: self
                .max_concurrent_connections
                .unwrap_or(defaults.max_concurrent_connections),
            max_concurrent_requests: self
                .max_concurrent_requests
                .unwrap_or(defaults.max_concurrent_requests),
            max_blocking_operations: self
                .max_blocking_operations
                .unwrap_or(defaults.max_blocking_operations),
            max_dump_streams: self.max_dump_streams.unwrap_or(defaults.max_dump_streams),
            blocking_timeout_ms: self
                .blocking_timeout_ms
                .unwrap_or(defaults.blocking_timeout_ms),
            header_timeout_ms: self.header_timeout_ms.unwrap_or(defaults.header_timeout_ms),
            global_requests_per_minute: self
                .global_requests_per_minute
                .unwrap_or(defaults.global_requests_per_minute),
            global_response_bytes_per_minute: self
                .global_response_bytes_per_minute
                .unwrap_or(defaults.global_response_bytes_per_minute),
            source_challenges_per_minute: self
                .source_challenges_per_minute
                .unwrap_or(defaults.source_challenges_per_minute),
            source_bindings_per_minute: self
                .source_bindings_per_minute
                .unwrap_or(defaults.source_bindings_per_minute),
            max_source_keys: self.max_source_keys.unwrap_or(defaults.max_source_keys),
        };
        if limits.max_concurrent_connections == 0
            || limits.max_concurrent_requests == 0
            || limits.max_blocking_operations == 0
            || limits.max_dump_streams == 0
            || limits.blocking_timeout_ms == 0
            || limits.header_timeout_ms == 0
            || limits.global_requests_per_minute == 0
            || limits.global_response_bytes_per_minute == 0
            || limits.source_challenges_per_minute == 0
            || limits.source_bindings_per_minute == 0
            || limits.max_source_keys == 0
        {
            return Err("registry HTTP limits must be nonzero".into());
        }
        if limits.max_concurrent_connections > MAX_REGISTRY_CONCURRENT_CONNECTIONS
            || limits.max_concurrent_requests > MAX_REGISTRY_CONCURRENT_REQUESTS
            || limits.max_blocking_operations > MAX_REGISTRY_BLOCKING_OPERATIONS
            || limits.max_dump_streams > MAX_REGISTRY_DUMP_STREAMS
            || limits.blocking_timeout_ms > MAX_REGISTRY_BLOCKING_TIMEOUT_MS
            || limits.header_timeout_ms > MAX_REGISTRY_HEADER_TIMEOUT_MS
            || limits.global_requests_per_minute > MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE
            || limits.global_response_bytes_per_minute > MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE
            || limits.source_challenges_per_minute > MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE
            || limits.source_bindings_per_minute > MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE
            || limits.max_source_keys > MAX_REGISTRY_SOURCE_KEYS
        {
            return Err("registry HTTP limits exceed audited product maxima".into());
        }
        let registration_defaults = RegistrationLimits::default();
        let registration = RegistrationLimits {
            global_bindings_per_minute: limits
                .global_requests_per_minute
                .min(MAX_REGISTRATION_BINDINGS_PER_MINUTE),
            account_bindings_per_minute: self
                .account_bindings_per_minute
                .unwrap_or(registration_defaults.account_bindings_per_minute),
            max_account_keys: self
                .max_account_keys
                .unwrap_or(registration_defaults.max_account_keys),
        };
        if registration.account_bindings_per_minute == 0
            || registration.account_bindings_per_minute > MAX_REGISTRATION_BINDINGS_PER_MINUTE
            || registration.max_account_keys == 0
            || registration.max_account_keys > 1_000_000
        {
            return Err(
                "registry account limits are outside the supported production bounds".into(),
            );
        }
        Ok((limits, registration))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryComplianceFile {
    registry: WitnessedRegistryFile,
    claim_trace: RegistryClaimTraceFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryClaimTraceFile {
    policy: TracePolicyFile,
    network_directory: PathBuf,
    identity_directory: PathBuf,
    network_signing_key_file: PathBuf,
    identity_signing_key_file: PathBuf,
    max_records_per_segment: u32,
    network_max_storage_gb: u64,
    identity_max_storage_gb: u64,
}

struct ResolvedCompliance {
    registry: WitnessedRegistryConfig,
    jurisdiction: Jurisdiction,
    capture_policy: pigeonpost_registry::claim_trace::ClaimCapturePolicy,
    retention_days: Option<u64>,
    network_directory: PathBuf,
    identity_directory: PathBuf,
    network_signing_key_file: PathBuf,
    identity_signing_key_file: PathBuf,
    max_records_per_segment: u32,
    planned_records_per_minute: u32,
    capacity_utc_epochs: u64,
    network_max_storage_bytes: u64,
    identity_max_storage_bytes: u64,
}

struct ResolvedRuntime {
    bind: SocketAddr,
    http: RegistryHttpConfig,
    registration_limits: RegistrationLimits,
    witnessing: Option<RegistryWitnessingFile>,
    compliance: Option<ResolvedCompliance>,
}

struct ComplianceSupervisor {
    cache: Arc<WitnessedRegistryKeyCache>,
    sink: Arc<SealedClaimTraceSink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderSecretSource {
    OwnerOnlyFile,
    DevelopmentEnvironment,
}

struct ProviderSecret {
    value: Zeroizing<String>,
    source: ProviderSecretSource,
}

impl std::fmt::Debug for ProviderSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Default)]
struct ProviderSettings {
    github_client_id: Option<String>,
    github_client_secret: Option<ProviderSecret>,
    google_client_id: Option<String>,
}

impl std::fmt::Debug for ProviderSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderSettings")
            .field("github_client_id", &self.github_client_id)
            .field("github_client_secret", &self.github_client_secret)
            .field("google_client_id", &self.google_client_id)
            .finish()
    }
}

#[cfg(test)]
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

pub(crate) struct ComplianceOperatorOptions {
    pub dir: PathBuf,
    pub origin: String,
    pub key_id: String,
    pub confirm_key_id: String,
    pub checkpoint_backup: PathBuf,
    pub witness_timeout_seconds: u64,
    pub confirm_offline: bool,
    pub execute: bool,
    pub json: bool,
}

struct ComplianceOperatorRuntime {
    _process_lock: RegistryProcessLock,
    registry: Arc<Registry>,
    supervisor: WitnessSupervisor,
    poll_interval: Duration,
}

struct RegistryProcessLock {
    _file: PrivateFile,
}

impl ProviderSettings {
    fn from_env(allow_development_secret_env: bool) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_sources(
            strict_nonempty_env("PIGEONPOST_GITHUB_CLIENT_ID")?,
            utf8_env("PIGEONPOST_GITHUB_CLIENT_SECRET")?,
            strict_nonempty_env("PIGEONPOST_GITHUB_CLIENT_SECRET_FILE")?.map(PathBuf::from),
            strict_nonempty_env("PIGEONPOST_GOOGLE_CLIENT_ID")?,
            allow_development_secret_env,
        )
    }

    fn from_sources(
        github_client_id: Option<String>,
        github_client_secret: Option<String>,
        github_client_secret_file: Option<PathBuf>,
        google_client_id: Option<String>,
        allow_development_secret_env: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if github_client_secret.is_some() && github_client_secret_file.is_some() {
            return Err(
                "PIGEONPOST_GITHUB_CLIENT_SECRET and PIGEONPOST_GITHUB_CLIENT_SECRET_FILE are mutually exclusive"
                    .into(),
            );
        }
        let github_client_secret = match (github_client_secret, github_client_secret_file) {
            (Some(secret), None) => {
                if !allow_development_secret_env {
                    return Err(
                        "direct provider-secret environment values are disabled; use PIGEONPOST_GITHUB_CLIENT_SECRET_FILE"
                            .into(),
                    );
                }
                Some(ProviderSecret::development_environment(secret)?)
            }
            (None, Some(path)) => {
                if !path.is_absolute() {
                    return Err(
                        "PIGEONPOST_GITHUB_CLIENT_SECRET_FILE must be an absolute path".into(),
                    );
                }
                Some(ProviderSecret {
                    value: read_owner_only_secret(&path)?,
                    source: ProviderSecretSource::OwnerOnlyFile,
                })
            }
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
        };
        Self::checked(github_client_id, github_client_secret, google_client_id)
    }

    #[cfg(test)]
    fn from_values(
        github_client_id: Option<String>,
        github_client_secret: Option<String>,
        google_client_id: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_sources(
            github_client_id,
            github_client_secret,
            None,
            google_client_id,
            true,
        )
    }

    fn checked(
        github_client_id: Option<String>,
        github_client_secret: Option<ProviderSecret>,
        google_client_id: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if github_client_id.is_some() != github_client_secret.is_some() {
            return Err(
                "GitHub identity mode requires PIGEONPOST_GITHUB_CLIENT_ID and exactly one secret source"
                    .into(),
            );
        }
        Ok(Self {
            github_client_id,
            github_client_secret,
            google_client_id,
        })
    }

    fn registration_enabled(&self, allow_mock: bool) -> bool {
        self.github_client_id.is_some() || self.google_client_id.is_some() || allow_mock
    }

    fn uses_development_secret_env(&self) -> bool {
        self.github_client_secret
            .as_ref()
            .is_some_and(|secret| secret.source == ProviderSecretSource::DevelopmentEnvironment)
    }
}

impl ProviderSecret {
    fn development_environment(value: String) -> Result<Self, Box<dyn std::error::Error>> {
        if value.is_empty()
            || value.len() as u64 > crate::runtime_config::MAX_PROVIDER_SECRET_BYTES
            || value.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
        {
            return Err(
                "development provider secret must be one bounded line of visible ASCII".into(),
            );
        }
        Ok(Self {
            value: Zeroizing::new(value),
            source: ProviderSecretSource::DevelopmentEnvironment,
        })
    }

    fn expose(&self) -> &str {
        self.value.as_str()
    }
}

struct RegistryClaimResolver {
    cache: Arc<WitnessedRegistryKeyCache>,
}

impl ClaimTraceKeyResolver for RegistryClaimResolver {
    fn readiness(&self, now_ms: u64) -> Result<(), ClaimTraceError> {
        TraceKeyResolver::readiness(self.cache.as_ref(), now_ms)
            .map_err(|_| ClaimTraceError::Unavailable)
    }

    fn resolve_trace_key(
        &self,
        purpose: CompliancePurpose,
        jurisdiction: Jurisdiction,
        at_ms: u64,
    ) -> Result<Option<ResolvedClaimTraceKey>, ClaimTraceError> {
        TraceKeyResolver::resolve_trace_key(self.cache.as_ref(), purpose, jurisdiction, at_ms)
            .map(|resolved| resolved.map(claim_key))
            .map_err(|_| ClaimTraceError::Unavailable)
    }
}

fn claim_key(key: ResolvedTraceKey) -> ResolvedClaimTraceKey {
    ResolvedClaimTraceKey {
        key_id: key.key_id,
        public_key: key.public_key,
        not_before_ms: key.not_before_ms,
        not_after_ms: key.not_after_ms,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn publish_compliance_key(
    options: ComplianceOperatorOptions,
    purpose: CompliancePurpose,
    jurisdiction: Jurisdiction,
    authority: &str,
    epoch_start_ms: u64,
    generation: u32,
    public_key: &str,
    not_after_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let supplied_id = validate_common_operator_options(&options)?;
    let authority = decode_lower_hex::<32>(
        authority,
        "authority must be exactly 32 lowercase hexadecimal bytes",
    )?;
    let constructed_id =
        ComplianceKeyId::new(purpose, jurisdiction, authority, epoch_start_ms, generation);
    if supplied_id != constructed_id {
        return Err("--key-id does not exactly match the typed purpose, jurisdiction, authority, epoch, and generation"
            .into());
    }
    let publication = ComplianceKeyPublish {
        key_id: constructed_id,
        public_key: public_key.to_owned(),
        not_before_ms: epoch_start_ms,
        not_after_ms,
        status: ComplianceKeyStatus::Active,
    };
    validate_operator_publication(&publication)?;

    if !options.execute {
        emit_operator_preview(&options, &publication, "publish");
        return Ok(());
    }
    let runtime = open_compliance_operator(&options)?;
    append_and_witness(runtime, &options, publication, "publish").await
}

pub(crate) async fn transition_compliance_key(
    options: ComplianceOperatorOptions,
    status: ComplianceKeyStatus,
) -> Result<(), Box<dyn std::error::Error>> {
    let key_id = validate_common_operator_options(&options)?;
    if !matches!(
        status,
        ComplianceKeyStatus::Retired | ComplianceKeyStatus::Revoked
    ) {
        return Err("a status transition must target retired or revoked".into());
    }
    if !options.execute {
        emit_transition_preview(&options, &key_id, status);
        return Ok(());
    }

    let runtime = open_compliance_operator(&options)?;
    let existing = runtime
        .registry
        .compliance_keys(&ComplianceKeyQuery {
            key_id: Some(key_id),
            at_ms: Some(now_ms()),
            include_inactive: true,
            ..Default::default()
        })?
        .into_iter()
        .next()
        .ok_or("the key id has no witnessed active publication; publish and witness it first")?;
    let current = existing.publication.status;
    let allowed = current == status
        || matches!(
            (current, status),
            (ComplianceKeyStatus::Active, ComplianceKeyStatus::Retired)
                | (ComplianceKeyStatus::Active, ComplianceKeyStatus::Revoked)
                | (ComplianceKeyStatus::Retired, ComplianceKeyStatus::Revoked)
        );
    if !allowed {
        return Err(format!(
            "invalid compliance-key transition {} -> {}",
            status_name(current),
            status_name(status)
        )
        .into());
    }
    let mut publication = existing.publication;
    publication.status = status;
    validate_operator_publication(&publication)?;
    append_and_witness(runtime, &options, publication, "transition").await
}

fn validate_common_operator_options(
    options: &ComplianceOperatorOptions,
) -> Result<ComplianceKeyId, Box<dyn std::error::Error>> {
    if options.key_id != options.confirm_key_id {
        return Err("--confirm-key-id must exactly repeat --key-id".into());
    }
    if options.witness_timeout_seconds == 0 || options.witness_timeout_seconds > 300 {
        return Err("witness timeout must be between one and 300 seconds".into());
    }
    let encoded = decode_lower_hex::<COMPLIANCE_KEY_ID_LEN>(
        &options.key_id,
        "key_id must be exactly 47 lowercase hexadecimal bytes",
    )?;
    let key_id = ComplianceKeyId::decode(&encoded)?;
    if key_id.jurisdiction == Jurisdiction::Test {
        return Err("the test jurisdiction cannot be published by the operator CLI".into());
    }
    if options.execute && !options.confirm_offline {
        return Err("--confirm-offline is required before an operator append".into());
    }
    Ok(key_id)
}

fn validate_operator_publication(
    publication: &ComplianceKeyPublish,
) -> Result<(), Box<dyn std::error::Error>> {
    let public_key = decode_lower_hex::<32>(
        &publication.public_key,
        "public_key must be exactly 32 lowercase hexadecimal bytes",
    )?;
    keys::x25519_agree(&Identity::from_seed([0x5a; 32]), &public_key)
        .map_err(|_| "public_key is a non-contributory X25519 point")?;

    validate_compliance_epoch(
        &publication.key_id,
        publication.not_before_ms,
        publication.not_after_ms,
    )?;

    LogEntry::compliance_key(0, publication.clone(), now_ms()).validate()?;
    Ok(())
}

fn open_compliance_operator(
    options: &ComplianceOperatorOptions,
) -> Result<ComplianceOperatorRuntime, Box<dyn std::error::Error>> {
    require_registry_persistent_custody()?;
    validate_checkpoint_backup(&options.dir, &options.checkpoint_backup)?;
    let process_lock = RegistryProcessLock::acquire(&options.dir)?;
    let database = options.dir.join("registry.db");

    let file: RegistryFile = load_optional_toml(&options.dir.join("registry.toml"))?
        .ok_or("registry.toml with a complete [witnessing] policy is required")?;
    let (_, registration_limits) = file.server.limits.resolve()?;
    let signing_key = load_checkpoint_key(&options.dir, true)?;
    let witnessing = file
        .witnessing
        .as_ref()
        .ok_or("the local compliance-key operator requires complete [witnessing] configuration")?
        .resolve(&options.origin, signing_key.verifying_key())?;
    let compliance = file
        .compliance
        .as_ref()
        .map(|compliance| {
            resolve_compliance(
                &options.dir,
                &options.origin,
                compliance,
                registration_limits.global_bindings_per_minute,
            )
        })
        .transpose()?;
    if compliance.as_ref().is_some_and(|compliance| {
        compliance.registry.registry_checkpoint_key != signing_key.verifying_key().to_bytes()
    }) {
        return Err(
            "[compliance.registry].registry_checkpoint_key must match checkpoint.key".into(),
        );
    }
    validate_witness_trust_alignment(compliance.as_ref(), Some(&witnessing))?;

    let database = database
        .to_str()
        .ok_or("non-UTF-8 registry database path")?;
    let registry = Registry::open_existing(
        database,
        RegistryConfig {
            origin: options.origin.clone(),
            signing_key,
            allow_mock_identities: false,
        },
    )?
    .with_witness_policy(witnessing.policy)?;
    registry.audit_storage()?;
    let registry = Arc::new(registry);
    let poll_interval = witnessing.poll_interval;
    let supervisor = WitnessSupervisor::new(
        Arc::clone(&registry),
        witnessing.clients,
        poll_interval,
        witnessing.failure_backoff_initial,
        witnessing.failure_backoff_max,
    )?;
    Ok(ComplianceOperatorRuntime {
        _process_lock: process_lock,
        registry,
        supervisor,
        poll_interval,
    })
}

impl RegistryProcessLock {
    fn acquire(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let path = dir.join("registry.lock");
        let (file, _) = PrivateFile::open_or_create(&path)?;
        file.verify_named()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file.descriptor().metadata()?.permissions().mode() & 0o7777 != 0o600 {
                return Err("registry.lock must be exactly mode 0600".into());
            }
        }
        FileExt::try_lock_exclusive(file.descriptor()).map_err(|_| {
            "registry is already running or another operator ceremony holds registry.lock"
        })?;
        file.verify_named()?;
        Ok(Self { _file: file })
    }
}

fn validate_checkpoint_backup(dir: &Path, backup: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !backup.is_absolute() {
        return Err("--checkpoint-backup must be an absolute path".into());
    }
    let live_path = dir.join("checkpoint.key");
    let live_file = PrivateFile::open_existing(&live_path)?;
    let backup_file = PrivateFile::open_existing(backup)?;
    live_file.verify_named()?;
    backup_file.verify_named()?;

    let registry_directory = live_file
        .normalized_path()
        .parent()
        .ok_or("checkpoint.key has no registry parent directory")?;
    if backup_file
        .normalized_path()
        .starts_with(registry_directory)
    {
        return Err("checkpoint backup must live outside the registry directory".into());
    }
    if live_file.same_object(&backup_file)? {
        return Err("checkpoint key and backup must be independent files".into());
    }

    let live = read_checkpoint_seed_descriptor(&live_file)?;
    let copy = read_checkpoint_seed_descriptor(&backup_file)?;
    if *live != *copy {
        return Err("checkpoint backup does not match checkpoint.key".into());
    }
    live_file.verify_named()?;
    backup_file.verify_named()?;
    Ok(())
}

fn read_checkpoint_seed_descriptor(
    file: &PrivateFile,
) -> Result<Zeroizing<[u8; 32]>, Box<dyn std::error::Error>> {
    const READ_BOUND: u64 = 33;
    file.verify_named()?;
    let metadata = file.descriptor().metadata()?;
    if !metadata.is_file() || metadata.len() != 32 {
        return Err("checkpoint key copies must contain exactly 32 bytes".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !matches!(metadata.permissions().mode() & 0o7777, 0o400 | 0o600) {
            return Err("checkpoint key copies must be exactly mode 0400 or 0600".into());
        }
    }

    let mut bytes = Zeroizing::new(Vec::with_capacity(33));
    let descriptor = file.descriptor();
    descriptor.take(READ_BOUND).read_to_end(&mut bytes)?;
    file.verify_named()?;
    if bytes.len() != 32 {
        return Err("checkpoint key copies must contain exactly 32 bytes".into());
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&bytes);
    if *seed == [0u8; 32] {
        return Err("checkpoint key copies must not contain the all-zero seed".into());
    }
    Ok(seed)
}

async fn append_and_witness(
    runtime: ComplianceOperatorRuntime,
    options: &ComplianceOperatorOptions,
    publication: ComplianceKeyPublish,
    operation: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = runtime
        .registry
        .publish_compliance_key_idempotent(publication.clone())?;
    let index = result.key.index;
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(options.witness_timeout_seconds);
    let mut status = runtime.registry.witness_publication_status()?;

    while status.published_size <= index {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, runtime.supervisor.sync_once(now_secs())).await {
            Ok(Ok(report)) => status = report.publication,
            Ok(Err(_)) => status = runtime.registry.witness_publication_status()?,
            Err(_) => break,
        }
        if status.published_size > index {
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(runtime.poll_interval.min(remaining)).await;
        status = runtime.registry.witness_publication_status()?;
    }

    let committed = runtime.registry.committed_head()?;
    let published = runtime.registry.head()?;
    let witnessed = status.published_size > index;
    emit_operator_result(
        options,
        &publication,
        operation,
        index,
        result.appended,
        &status,
        &committed.root,
        &published.root,
        witnessed,
    );
    if !witnessed {
        return Err(
            "compliance-key leaf is durably committed but not witnessed; rerun the exact command to resume publication"
                .into(),
        );
    }
    Ok(())
}

fn emit_operator_preview(
    options: &ComplianceOperatorOptions,
    publication: &ComplianceKeyPublish,
    operation: &'static str,
) {
    let record = serde_json::json!({
        "version": 1,
        "mode": "dry_run",
        "operation": operation,
        "origin": options.origin,
        "key_id": options.key_id,
        "purpose": purpose_name(publication.key_id.purpose),
        "jurisdiction": jurisdiction_name(publication.key_id.jurisdiction),
        "authority": hex(&publication.key_id.authority),
        "epoch_start_ms": publication.not_before_ms,
        "not_after_ms": publication.not_after_ms,
        "generation": publication.key_id.generation,
        "public_key": publication.public_key,
        "status": status_name(publication.status),
        "database_opened": false,
    });
    print_operator_record(options.json, &record);
}

fn emit_transition_preview(
    options: &ComplianceOperatorOptions,
    key_id: &ComplianceKeyId,
    status: ComplianceKeyStatus,
) {
    let record = serde_json::json!({
        "version": 1,
        "mode": "dry_run",
        "operation": "transition",
        "origin": options.origin,
        "key_id": options.key_id,
        "purpose": purpose_name(key_id.purpose),
        "jurisdiction": jurisdiction_name(key_id.jurisdiction),
        "authority": hex(&key_id.authority),
        "epoch_start_ms": key_id.epoch_start_ms,
        "generation": key_id.generation,
        "status": status_name(status),
        "database_opened": false,
    });
    print_operator_record(options.json, &record);
}

#[allow(clippy::too_many_arguments)]
fn emit_operator_result(
    options: &ComplianceOperatorOptions,
    publication: &ComplianceKeyPublish,
    operation: &'static str,
    index: u64,
    appended: bool,
    status: &pigeonpost_registry::WitnessPublicationStatus,
    committed_root: &[u8; 32],
    published_root: &[u8; 32],
    witnessed: bool,
) {
    let record = serde_json::json!({
        "version": 1,
        "mode": "execute",
        "result": if witnessed { "witnessed" } else { "committed_unwitnessed" },
        "operation": operation,
        "origin": options.origin,
        "key_id": options.key_id,
        "purpose": purpose_name(publication.key_id.purpose),
        "jurisdiction": jurisdiction_name(publication.key_id.jurisdiction),
        "authority": hex(&publication.key_id.authority),
        "epoch_start_ms": publication.not_before_ms,
        "not_after_ms": publication.not_after_ms,
        "generation": publication.key_id.generation,
        "public_key": publication.public_key,
        "status": status_name(publication.status),
        "log_index": index,
        "appended": appended,
        "committed_size": status.committed_size,
        "published_size": status.published_size,
        "witnessed_at": status.witnessed_at,
        "committed_root": hex(committed_root),
        "published_root": hex(published_root),
    });
    print_operator_record(options.json, &record);
}

fn print_operator_record(json: bool, record: &serde_json::Value) {
    if json {
        println!("{record}");
        return;
    }
    let object = record
        .as_object()
        .expect("operator records are always JSON objects");
    for key in [
        "mode",
        "result",
        "operation",
        "origin",
        "key_id",
        "purpose",
        "jurisdiction",
        "authority",
        "epoch_start_ms",
        "not_after_ms",
        "generation",
        "public_key",
        "status",
        "log_index",
        "appended",
        "committed_size",
        "published_size",
        "witnessed_at",
        "committed_root",
        "published_root",
        "database_opened",
    ] {
        if let Some(value) = object.get(key) {
            let rendered = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            println!("{key}={rendered}");
        }
    }
}

fn decode_lower_hex<const N: usize>(
    input: &str,
    message: &'static str,
) -> Result<[u8; N], Box<dyn std::error::Error>> {
    if input.len() != N * 2
        || input
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(message.into());
    }
    let mut output = [0u8; N];
    for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
        output[index] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?;
    }
    Ok(output)
}

const fn purpose_name(purpose: CompliancePurpose) -> &'static str {
    match purpose {
        CompliancePurpose::Attribution => "attribution",
        CompliancePurpose::NetworkTrace => "network_trace",
        CompliancePurpose::IdentityTrace => "identity_trace",
    }
}

const fn jurisdiction_name(jurisdiction: Jurisdiction) -> &'static str {
    match jurisdiction {
        Jurisdiction::Us => "us",
        Jurisdiction::Eu => "eu",
        Jurisdiction::Tr => "tr",
        Jurisdiction::Test => "test",
    }
}

const fn status_name(status: ComplianceKeyStatus) -> &'static str {
    match status {
        ComplianceKeyStatus::Active => "active",
        ComplianceKeyStatus::Retired => "retired",
        ComplianceKeyStatus::Revoked => "revoked",
    }
}

pub async fn serve(
    bind: &str,
    dir: &Path,
    origin: &str,
    legacy_checkpoint: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Keep the full implementation type-checked on every target, but reject unsupported targets
    // before creating the root, process lock, checkpoint key, database, or SQLite sidecars.
    require_registry_persistent_custody()?;
    let storage_root = PrivateDirectory::open_or_create(dir)?;
    let dir = storage_root.normalized_path();
    let allow_development_secret_env = binary_env("PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV")?;
    let providers = ProviderSettings::from_env(allow_development_secret_env)?;
    let allow_mock = test_mock_identities_enabled()?;
    let runtime = resolve_runtime(dir, bind, origin, &providers, allow_mock)?;

    let trace_seeds = runtime
        .compliance
        .as_ref()
        .map(|compliance| {
            Ok::<_, Box<dyn std::error::Error>>((
                read_existing_seed(dir, &compliance.network_signing_key_file)?,
                read_existing_seed(dir, &compliance.identity_signing_key_file)?,
            ))
        })
        .transpose()?;
    if trace_seeds
        .as_ref()
        .is_some_and(|(network, identity)| **network == **identity)
    {
        return Err("network and identity claim-trace signing keys must be distinct".into());
    }

    let _process_lock = RegistryProcessLock::acquire(dir)?;
    let signing_key = load_checkpoint_key(
        dir,
        runtime.compliance.is_some() || runtime.witnessing.is_some(),
    )?;
    validate_checkpoint_identity(&runtime, &signing_key)?;
    let witnessing = runtime
        .witnessing
        .as_ref()
        .map(|witnessing| witnessing.resolve(origin, signing_key.verifying_key()))
        .transpose()?;
    validate_witness_trust_alignment(runtime.compliance.as_ref(), witnessing.as_ref())?;
    if trace_seeds.as_ref().is_some_and(|(network, identity)| {
        &**network == signing_key.as_bytes() || &**identity == signing_key.as_bytes()
    }) {
        return Err(
            "claim-trace signing keys must be separate from the registry checkpoint key".into(),
        );
    }

    let database = dir.join("registry.db");
    let database = database.to_str().ok_or("non-UTF-8 path")?;
    let config = RegistryConfig {
        origin: origin.to_string(),
        signing_key,
        allow_mock_identities: allow_mock,
    };
    let mut registry = match legacy_checkpoint {
        Some(path) => {
            let note = read_legacy_checkpoint(path)?;
            Registry::open_with_legacy_checkpoint(database, config, &note)?
        }
        None => Registry::open(database, config)?,
    }
    .with_registration_limits(runtime.registration_limits)?;
    storage_root.verify_named()?;

    let mut provider_names = Vec::new();
    if let (Some(id), Some(secret)) = (providers.github_client_id, providers.github_client_secret) {
        registry = registry.with_provider(Box::new(GithubProvider::new(
            id,
            secret.expose().to_owned(),
        )));
        provider_names.push("github");
    }
    if let Some(id) = providers.google_client_id {
        registry = registry.with_provider(Box::new(GoogleProvider::new(id)));
        provider_names.push("google");
    }
    #[cfg(feature = "test-utilities")]
    if allow_mock && !provider_names.contains(&"github") {
        registry = registry.with_provider(Box::new(pigeonpost_registry::identity::MockProvider));
        provider_names.push("mock");
    }

    let mut compliance_supervisor = None;
    if let (Some(compliance), Some((network_seed, identity_seed))) =
        (runtime.compliance, trace_seeds)
    {
        let cache = WitnessedRegistryKeyCache::new(compliance.registry)
            .map_err(|_| "invalid witnessed compliance registry configuration")?;
        let resolver: Arc<dyn ClaimTraceKeyResolver> = Arc::new(RegistryClaimResolver {
            cache: Arc::clone(&cache),
        });
        let sink = Arc::new(
            SealedClaimTraceSink::new(
                SealedClaimTraceConfig {
                    network_directory: compliance.network_directory,
                    identity_directory: compliance.identity_directory,
                    jurisdiction: compliance.jurisdiction,
                    node_id: registry.verifying_key().to_bytes(),
                    capture_policy: compliance.capture_policy,
                    retention_days: compliance.retention_days,
                    max_records_per_segment: compliance.max_records_per_segment,
                    planned_records_per_minute: compliance.planned_records_per_minute,
                    capacity_utc_epochs: compliance.capacity_utc_epochs,
                    network_max_storage_bytes: compliance.network_max_storage_bytes,
                    identity_max_storage_bytes: compliance.identity_max_storage_bytes,
                },
                resolver,
                *network_seed,
                *identity_seed,
            )
            .map_err(|_| "registry claim-trace configuration is unavailable")?,
        );
        if cache.refresh_once().await.is_err() && sink.readiness(now_ms()).is_err() {
            return Err("initial witnessed compliance-key refresh failed".into());
        }
        sink.readiness(now_ms())
            .map_err(|_| "no current witnessed network/identity trace keys are available")?;
        registry = registry.with_claim_trace(sink.clone());
        compliance_supervisor = Some(ComplianceSupervisor { cache, sink });
    }
    if let Some(witnessing) = witnessing.as_ref() {
        registry = registry.with_witness_policy(witnessing.policy.clone())?;
    }
    let registry = Arc::new(registry);
    let witness_supervisor = witnessing
        .map(|witnessing| {
            WitnessSupervisor::new(
                Arc::clone(&registry),
                witnessing.clients,
                witnessing.poll_interval,
                witnessing.failure_backoff_initial,
                witnessing.failure_backoff_max,
            )
        })
        .transpose()?;
    if let Some(supervisor) = witness_supervisor.as_ref() {
        // A fresh deployment has no published head until the first durable quorum. On restart, a
        // transient witness outage is tolerated only while the stored head is still ready.
        let _ = supervisor.sync_once(now_secs()).await;
    }
    registry.registration_readiness(now_ms())?;

    let listener = tokio::net::TcpListener::bind(runtime.bind).await?;
    println!("registry listening on {}", listener.local_addr()?);
    println!("  origin      {origin}");
    println!("  storage     {}", dir.display());
    println!("  entries     {}", registry.size()?);
    println!("  checkpoint  {}", hex(registry.verifying_key().as_bytes()));
    if provider_names.is_empty() {
        println!("  providers   none — resolve and dump work; registration does not");
    } else {
        println!("  providers   {}", provider_names.join(", "));
    }
    if allow_mock {
        println!();
        println!("  !! MOCK IDENTITIES ENABLED — anyone can claim any handle.");
        println!("  !! Loopback test mode only.");
        println!();
    }
    run_supervised(
        listener,
        registry,
        runtime.http,
        compliance_supervisor,
        witness_supervisor,
        allow_mock,
    )
    .await?;

    storage_root.verify_named()?;
    println!("registry stopped");
    Ok(())
}

fn require_registry_persistent_custody() -> Result<(), Box<dyn std::error::Error>> {
    require_registry_persistent_custody_for(cfg!(any(target_os = "linux", target_os = "macos")))
}

fn require_registry_persistent_custody_for(
    supported: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if supported {
        Ok(())
    } else {
        // Registry persistence has not yet established complete custody for its SQLite database
        // and sidecars on this platform. Executed operator ceremonies use the same storage.
        Err("registry persistence requires verified Linux or macOS storage custody".into())
    }
}

fn resolve_runtime(
    dir: &Path,
    bind: &str,
    origin: &str,
    providers: &ProviderSettings,
    allow_mock: bool,
) -> Result<ResolvedRuntime, Box<dyn std::error::Error>> {
    let bind: SocketAddr = bind
        .parse()
        .map_err(|_| "bind must be an IP socket address such as 127.0.0.1:7718")?;
    if bind.port() == 0 {
        return Err("bind port must not be zero".into());
    }
    if allow_mock && !bind.ip().is_loopback() {
        return Err("mock identities may listen only on loopback".into());
    }
    if providers.uses_development_secret_env() && !bind.ip().is_loopback() {
        return Err(
            "direct provider-secret environment values are restricted to explicit loopback development"
                .into(),
        );
    }

    let file: RegistryFile = load_optional_toml(&dir.join("registry.toml"))?.unwrap_or_default();
    let (limits, registration_limits) = file.server.limits.resolve()?;
    let directory_publishers = resolve_directory_publishers(&file.server.directory_publisher_keys)?;
    let has_directory_publishers = !directory_publishers.is_empty();
    let http = RegistryHttpConfig::with_trusted_proxies(file.server.trusted_proxies)?
        .with_directory_publishers(directory_publishers)?
        .with_limits(limits)?;
    let compliance = file
        .compliance
        .as_ref()
        .map(|compliance| {
            resolve_compliance(
                dir,
                origin,
                compliance,
                registration_limits.global_bindings_per_minute,
            )
        })
        .transpose()?;
    let registration_enabled = providers.registration_enabled(allow_mock);
    if registration_enabled && compliance.is_none() {
        return Err(
            "identity-provider mode requires complete [compliance.registry] and [compliance.claim_trace] configuration"
                .into(),
        );
    }
    if (registration_enabled || !bind.ip().is_loopback()) && file.witnessing.is_none() {
        return Err(
            "identity-provider and public registry modes require complete [witnessing] configuration"
                .into(),
        );
    }
    if file.witnessing.is_some() && !has_directory_publishers {
        return Err(
            "witnessed registry mode requires server.directory_publisher_keys with at least one pinned Ed25519 public key"
                .into(),
        );
    }
    Ok(ResolvedRuntime {
        bind,
        http,
        registration_limits,
        witnessing: file.witnessing,
        compliance,
    })
}

fn resolve_directory_publishers(
    encoded: &[String],
) -> Result<Vec<ed25519_dalek::VerifyingKey>, Box<dyn std::error::Error>> {
    if encoded.len() > 64 {
        return Err("at most 64 directory publisher keys may be configured".into());
    }
    let mut unique = HashSet::with_capacity(encoded.len());
    let mut publishers = Vec::with_capacity(encoded.len());
    for value in encoded {
        if value.len() != 64
            || value
                .bytes()
                .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        {
            return Err(
                "directory publisher keys must be exactly 32 lowercase hexadecimal bytes".into(),
            );
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?;
        }
        if bytes == [0u8; 32] || !unique.insert(bytes) {
            return Err("directory publisher keys must be nonzero and unique".into());
        }
        publishers.push(
            keys::verifying_key_from_bytes(&bytes)
                .map_err(|_| "directory publisher key is not a valid Ed25519 public key")?,
        );
    }
    Ok(publishers)
}

fn validate_checkpoint_identity(
    runtime: &ResolvedRuntime,
    signing_key: &SigningKey,
) -> Result<(), Box<dyn std::error::Error>> {
    if runtime.compliance.as_ref().is_some_and(|compliance| {
        compliance.registry.registry_checkpoint_key != signing_key.verifying_key().to_bytes()
    }) {
        return Err(
            "[compliance.registry].registry_checkpoint_key must match checkpoint.key".into(),
        );
    }
    Ok(())
}

fn validate_witness_trust_alignment(
    compliance: Option<&ResolvedCompliance>,
    witnessing: Option<&ResolvedRegistryWitnessing>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(compliance), Some(witnessing)) = (compliance, witnessing) else {
        return Ok(());
    };
    let readers = &compliance.registry.witnesses;
    let publishers = witnessing.policy.witnesses();
    let same_witnesses = readers.len() == publishers.len()
        && publishers.iter().all(|publisher| {
            readers.iter().any(|reader| {
                reader.name == publisher.name()
                    && reader.public_key.as_slice() == publisher.key().as_bytes()
            })
        });
    if !same_witnesses || compliance.registry.witness_threshold != witnessing.policy.threshold() {
        return Err(
            "[witnessing] and [compliance.registry] must use the same witness set and threshold"
                .into(),
        );
    }
    Ok(())
}

fn resolve_compliance(
    dir: &Path,
    origin: &str,
    compliance: &RegistryComplianceFile,
    records_per_minute: u32,
) -> Result<ResolvedCompliance, Box<dyn std::error::Error>> {
    validate_segment_limit(compliance.claim_trace.max_records_per_segment)?;
    let network_max_storage_bytes = resolve_trace_storage_gib(
        "compliance.claim_trace.network_max_storage_gb",
        compliance.claim_trace.network_max_storage_gb,
    )?;
    let identity_max_storage_bytes = resolve_trace_storage_gib(
        "compliance.claim_trace.identity_max_storage_gb",
        compliance.claim_trace.identity_max_storage_gb,
    )?;
    let retention_days = compliance.claim_trace.policy.effective_retention_days()?;
    let capacity_epochs = compliance.claim_trace.policy.capacity_epochs()?;
    let network_required_bytes = required_network_trace_storage_bytes(
        records_per_minute,
        capacity_epochs,
        compliance.claim_trace.max_records_per_segment,
    )
    .map_err(|_| {
        "compliance.claim_trace network sizing cannot satisfy the configured admission rate and capacity runway"
    })?;
    let identity_required_bytes = required_identity_trace_storage_bytes(
        records_per_minute,
        capacity_epochs,
        compliance.claim_trace.max_records_per_segment,
    )
    .map_err(|_| {
        "compliance.claim_trace identity sizing cannot satisfy the configured admission rate and capacity runway"
    })?;
    validate_trace_storage_requirement(
        "compliance.claim_trace.network_max_storage_gb",
        network_max_storage_bytes,
        network_required_bytes,
    )?;
    validate_trace_storage_requirement(
        "compliance.claim_trace.identity_max_storage_gb",
        identity_max_storage_bytes,
        identity_required_bytes,
    )?;
    let registry = compliance.registry.resolve(dir)?;
    if registry.expected_origin != origin {
        return Err("compliance registry expected_origin must match --origin".into());
    }
    let network_directory = resolve_path(dir, &compliance.claim_trace.network_directory)?;
    let identity_directory = resolve_path(dir, &compliance.claim_trace.identity_directory)?;
    validate_separate_directories(&network_directory, &identity_directory)?;
    Ok(ResolvedCompliance {
        registry,
        jurisdiction: compliance.claim_trace.policy.jurisdiction,
        capture_policy: compliance.claim_trace.policy.registry_policy()?,
        retention_days,
        network_directory,
        identity_directory,
        network_signing_key_file: compliance.claim_trace.network_signing_key_file.clone(),
        identity_signing_key_file: compliance.claim_trace.identity_signing_key_file.clone(),
        max_records_per_segment: compliance.claim_trace.max_records_per_segment,
        planned_records_per_minute: records_per_minute,
        capacity_utc_epochs: capacity_epochs,
        network_max_storage_bytes,
        identity_max_storage_bytes,
    })
}

async fn run_supervised(
    listener: tokio::net::TcpListener,
    registry: Arc<Registry>,
    http: RegistryHttpConfig,
    compliance: Option<ComplianceSupervisor>,
    witnessing: Option<WitnessSupervisor>,
    source_test_mock: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let witnessed = registry.witness_policy().is_some();
    let server_registry = Arc::clone(&registry);
    let (server_stop_tx, server_stop_rx) = tokio::sync::watch::channel(false);
    let supervisors = (compliance, witnessing);
    let mut server_task = tokio::spawn(async move {
        #[cfg(feature = "test-utilities")]
        if source_test_mock {
            return serve_loopback_test_supervised(
                listener,
                server_registry,
                http,
                server_stop_rx,
                move |background_stop| async move {
                    supervise_background(supervisors.0, supervisors.1, background_stop)
                        .await
                        .map_err(|()| RegistryError::RegistryUnavailable)
                },
            )
            .await;
        }
        #[cfg(not(feature = "test-utilities"))]
        let _ = source_test_mock;
        if witnessed {
            serve_witnessed(
                listener,
                server_registry,
                http,
                server_stop_rx,
                move |background_stop| async move {
                    supervise_background(supervisors.0, supervisors.1, background_stop)
                        .await
                        .map_err(|()| RegistryError::RegistryUnavailable)
                },
            )
            .await
        } else {
            drop(supervisors);
            serve_loopback_read_only(listener, server_registry, http, server_stop_rx).await
        }
    });

    enum Stop {
        Requested,
        Server(Result<pigeonpost_registry::Result<()>, tokio::task::JoinError>),
    }
    let stop = tokio::select! {
        _ = crate::output::shutdown_signal() => Stop::Requested,
        result = &mut server_task => Stop::Server(result),
    };

    let _ = server_stop_tx.send(true);
    match stop {
        // The registry serve boundary bounds HTTP/background drain, then performs an intentionally
        // unbounded claim-trace queue drain and worker join. Await it; never abort durability work.
        Stop::Requested => server_outcome((&mut server_task).await),
        Stop::Server(result) => match server_outcome(result) {
            Ok(()) => Err("registry server stopped unexpectedly".into()),
            Err(error) => Err(error),
        },
    }
}

fn server_outcome(
    result: Result<pigeonpost_registry::Result<()>, tokio::task::JoinError>,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err("registry server task failed".into()),
    }
}

async fn supervise_background(
    compliance: Option<ComplianceSupervisor>,
    witnessing: Option<WitnessSupervisor>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), ()> {
    match (compliance, witnessing) {
        (Some(compliance), Some(witnessing)) => {
            let compliance_stop = stop.clone();
            tokio::select! {
                result = refresh_until_stopped(compliance, compliance_stop) => result,
                result = witnessing.run(stop) => result.map_err(|_| ()),
            }
        }
        (Some(compliance), None) => refresh_until_stopped(compliance, stop).await,
        (None, Some(witnessing)) => witnessing.run(stop).await.map_err(|_| ()),
        (None, None) => Ok(()),
    }
}

async fn refresh_until_stopped(
    supervisor: ComplianceSupervisor,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), ()> {
    let interval_ms =
        AttributionKeyResolver::refresh_interval_ms(supervisor.cache.as_ref()).ok_or(())?;
    loop {
        tokio::select! {
            changed = stop.changed() => {
                changed.map_err(|_| ())?;
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {
                let _ = supervisor.cache.refresh_once().await;
                if supervisor.sink.readiness(now_ms()).is_err() {
                    return Err(());
                }
            }
        }
    }
}

fn strict_nonempty_env(name: &'static str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(value) = utf8_env(name)? else {
        return Ok(None);
    };
    Ok((!value.trim().is_empty()).then_some(value))
}

fn utf8_env(name: &'static str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| format!("{name} must contain valid UTF-8").into())
        })
        .transpose()
}

fn binary_env(name: &'static str) -> Result<bool, Box<dyn std::error::Error>> {
    match strict_nonempty_env(name)?.as_deref() {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(format!("{name} must be 0 or 1").into()),
    }
}

#[cfg(feature = "test-utilities")]
fn test_mock_identities_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    if binary_env("PIGEONPOST_ALLOW_MOCK_IDENTITIES")? {
        return Err(
            "PIGEONPOST_ALLOW_MOCK_IDENTITIES is retired; use the source-test-only PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES flag"
                .into(),
        );
    }
    binary_env("PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES")
}

#[cfg(not(feature = "test-utilities"))]
fn test_mock_identities_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    let legacy = binary_env("PIGEONPOST_ALLOW_MOCK_IDENTITIES")?;
    let test_only = binary_env("PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES")?;
    if legacy || test_only {
        return Err("mock identities are not compiled into production Pigeonpost binaries".into());
    }
    Ok(false)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn load_checkpoint_key(
    dir: &Path,
    require_existing: bool,
) -> Result<SigningKey, Box<dyn std::error::Error>> {
    if require_existing {
        let seed = read_existing_seed(dir, Path::new("checkpoint.key"))?;
        Ok(SigningKey::from_bytes(&seed))
    } else {
        load_or_create_key(&dir.join("checkpoint.key"))
    }
}

fn read_legacy_checkpoint(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    const MAX_BYTES: u64 = 64 * 1024;
    let bytes = read_trusted_file_bounded(path, MAX_BYTES)?;
    let note = std::str::from_utf8(&bytes)
        .map_err(|_| "legacy checkpoint must be valid UTF-8")?
        .to_owned();
    Ok(note)
}

/// The checkpoint key. Losing it means witnesses stop recognising our signatures, so it is the
/// one file in a registry deployment that must be backed up.
fn load_or_create_key(path: &Path) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let (seed, created) = crate::loft_key::load_or_create_seed(path)?;
    let key = SigningKey::from_bytes(&seed);
    if created {
        println!("generated a new checkpoint key at {}", path.display());
        println!("BACK THIS UP — witnesses recognise the registry by it.");
    }
    Ok(key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_persistent_custody_platform_guard_is_fail_closed() {
        assert!(require_registry_persistent_custody_for(false).is_err());
        require_registry_persistent_custody_for(true).unwrap();
    }

    fn key(seed: u8) -> String {
        hex(SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes())
    }

    #[test]
    fn partial_provider_credentials_fail_closed() {
        let error = ProviderSettings::from_values(Some("id".into()), None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("exactly one secret source"));
    }

    #[test]
    fn provider_settings_debug_and_errors_never_expose_secrets() {
        let canary = "provider-secret-canary-7f91";
        let settings = ProviderSettings::from_values(
            Some("public-client-id".into()),
            Some(canary.into()),
            None,
        )
        .unwrap();
        let rendered = format!("{settings:?}");
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains(canary));

        let error = ProviderSettings::from_sources(
            Some("public-client-id".into()),
            Some(canary.into()),
            Some(PathBuf::from("/run/secrets/provider")),
            None,
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("mutually exclusive"));
        assert!(!error.contains(canary));

        let error = ProviderSettings::from_sources(
            Some("public-client-id".into()),
            Some(String::new()),
            Some(PathBuf::from("/run/secrets/provider")),
            None,
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("mutually exclusive"));

        let error = ProviderSettings::from_values(
            Some("public-client-id".into()),
            Some(format!("{canary}\n")),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains(canary));
    }

    #[test]
    fn provider_secret_files_are_bounded_single_link_owner_only_regular_files() {
        let directory = crate::test_support::private_tempdir();
        let path = directory.path().join("github-client-secret");
        let canary = "provider-file-canary-4c28";
        crate::test_support::write_private(&path, canary).unwrap();
        set_owner_only(&path);

        let settings = ProviderSettings::from_sources(
            Some("public-client-id".into()),
            None,
            Some(path.clone()),
            None,
            false,
        )
        .unwrap();
        let secret = settings.github_client_secret.as_ref().unwrap();
        assert_eq!(secret.source, ProviderSecretSource::OwnerOnlyFile);
        assert_eq!(secret.expose(), canary);
        assert!(!format!("{settings:?}").contains(canary));

        crate::test_support::write_private(
            &path,
            vec![b'x'; crate::runtime_config::MAX_PROVIDER_SECRET_BYTES as usize + 1],
        )
        .unwrap();
        assert!(ProviderSettings::from_sources(
            Some("public-client-id".into()),
            None,
            Some(path.clone()),
            None,
            false,
        )
        .is_err());

        crate::test_support::write_private(&path, format!("{canary}\n")).unwrap();
        assert!(ProviderSettings::from_sources(
            Some("public-client-id".into()),
            None,
            Some(path.clone()),
            None,
            false,
        )
        .is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};

            crate::test_support::write_private(&path, canary).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(ProviderSettings::from_sources(
                Some("public-client-id".into()),
                None,
                Some(path.clone()),
                None,
                false,
            )
            .is_err());

            set_owner_only(&path);
            let hardlink = directory.path().join("hardlink");
            std::fs::hard_link(&path, &hardlink).unwrap();
            assert!(ProviderSettings::from_sources(
                Some("public-client-id".into()),
                None,
                Some(path.clone()),
                None,
                false,
            )
            .is_err());
            std::fs::remove_file(hardlink).unwrap();

            let linked = directory.path().join("linked-secret");
            symlink(&path, &linked).unwrap();
            assert!(ProviderSettings::from_sources(
                Some("public-client-id".into()),
                None,
                Some(linked),
                None,
                false,
            )
            .is_err());
        }
    }

    #[cfg(unix)]
    fn set_owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(not(unix))]
    fn set_owner_only(_path: &Path) {}

    #[test]
    fn development_secret_environment_is_explicit_and_loopback_only() {
        let directory = crate::test_support::private_tempdir();
        let error = ProviderSettings::from_sources(
            Some("public-client-id".into()),
            Some("development-secret".into()),
            None,
            None,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("disabled"));

        let providers = ProviderSettings::from_values(
            Some("public-client-id".into()),
            Some("development-secret".into()),
            None,
        )
        .unwrap();
        let error = resolve_runtime(
            directory.path(),
            "0.0.0.0:7718",
            "registry.example/log",
            &providers,
            false,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("loopback development"));
    }

    #[test]
    fn identity_provider_mode_requires_compliance_before_storage_or_binding() {
        let dir = crate::test_support::private_tempdir();
        let providers =
            ProviderSettings::from_values(None, None, Some("google-client".into())).unwrap();
        let error = resolve_runtime(
            dir.path(),
            "127.0.0.1:7718",
            "registry.example/log",
            &providers,
            false,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("requires complete"));
        assert!(!dir.path().join("checkpoint.key").exists());
        assert!(!dir.path().join("registry.db").exists());
    }

    #[test]
    fn compliance_mode_never_invents_a_checkpoint_identity() {
        let dir = crate::test_support::private_tempdir();
        let error = load_checkpoint_key(dir.path(), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("checkpoint.key"));
        assert!(!dir.path().join("checkpoint.key").exists());
    }

    #[test]
    fn mock_identity_mode_is_confined_to_loopback() {
        let error = resolve_runtime(
            Path::new("."),
            "0.0.0.0:7718",
            "registry.example/log",
            &ProviderSettings::default(),
            true,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("loopback"));
    }

    #[test]
    fn public_registry_mode_requires_witness_submission_configuration() {
        let dir = crate::test_support::private_tempdir();
        let error = resolve_runtime(
            dir.path(),
            "0.0.0.0:7718",
            "registry.example/log",
            &ProviderSettings::default(),
            false,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("[witnessing]"));
        assert!(!dir.path().join("checkpoint.key").exists());
        assert!(!dir.path().join("registry.db").exists());
    }

    #[test]
    fn complete_registry_configuration_builds_bounded_http_and_trace_runtime() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("registry.toml"),
            format!(
                r#"[server]
trusted_proxies = ["127.0.0.1"]
directory_publisher_keys = ["{}"]

[server.limits]
max_concurrent_connections = 64
max_concurrent_requests = 32
max_blocking_operations = 4
max_dump_streams = 2
blocking_timeout_ms = 10000
header_timeout_ms = 5000
global_requests_per_minute = 1000
source_challenges_per_minute = 10
source_bindings_per_minute = 5
max_source_keys = 1024
account_bindings_per_minute = 3
max_account_keys = 512

[witnessing]
threshold = 2
max_cosignature_age_seconds = 600
max_lag_entries = 0
poll_interval_seconds = 30

[[witnessing.witnesses]]
name = "one"
public_key = "{}"
submission_prefix = "https://witness-one.example/submission/"
monitoring_prefix = "https://witness-one.example/monitoring/"

[[witnessing.witnesses]]
name = "two"
public_key = "{}"
submission_prefix = "https://witness-two.example/submission/"
monitoring_prefix = "https://witness-two.example/monitoring/"

[compliance.registry]
registry_url = "https://registry.example/"
expected_origin = "registry.example/log"
registry_checkpoint_key = "{}"
witness_threshold = 2
minimum_checkpoint_size = 1
minimum_checkpoint_root = "{}"
max_staleness_seconds = 600
refresh_interval_seconds = 60
state_path = "compliance/registry-state.json"

[[compliance.registry.witnesses]]
name = "one"
public_key = "{}"

[[compliance.registry.witnesses]]
name = "two"
public_key = "{}"

[compliance.claim_trace]
network_directory = "compliance/network"
identity_directory = "compliance/identity"
network_signing_key_file = "secrets/network.key"
identity_signing_key_file = "secrets/identity.key"
max_records_per_segment = 1000
network_max_storage_gb = 10000
identity_max_storage_gb = 10000

[compliance.claim_trace.policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 730
	"#,
                key(4),
                key(2),
                key(3),
                key(1),
                "44".repeat(32),
                key(2),
                key(3),
            ),
        )
        .unwrap();

        let providers =
            ProviderSettings::from_values(None, None, Some("google-client".into())).unwrap();
        let resolved = resolve_runtime(
            dir.path(),
            "127.0.0.1:7718",
            "registry.example/log",
            &providers,
            false,
        )
        .unwrap();
        assert_eq!(resolved.registration_limits.account_bindings_per_minute, 3);
        assert_eq!(resolved.registration_limits.max_account_keys, 512);
        let matching = SigningKey::from_bytes(&[1u8; 32]);
        let witnessing = resolved
            .witnessing
            .as_ref()
            .unwrap()
            .resolve("registry.example/log", matching.verifying_key())
            .unwrap();
        let compliance = resolved.compliance.unwrap();
        assert!(validate_witness_trust_alignment(Some(&compliance), Some(&witnessing)).is_ok());
        assert_eq!(
            compliance.registry.state_path,
            dir.path().join("compliance/registry-state.json")
        );
        assert_eq!(
            compliance.identity_directory,
            dir.path().join("compliance/identity")
        );
        assert_eq!(
            compliance.network_max_storage_bytes,
            10_000 * 1024 * 1024 * 1024
        );
        assert_eq!(
            compliance.identity_max_storage_bytes,
            10_000 * 1024 * 1024 * 1024
        );

        let runtime = ResolvedRuntime {
            bind: "127.0.0.1:7718".parse().unwrap(),
            http: RegistryHttpConfig::default(),
            registration_limits: RegistrationLimits::default(),
            witnessing: None,
            compliance: Some(compliance),
        };
        assert!(validate_checkpoint_identity(&runtime, &matching).is_ok());
        assert!(
            validate_checkpoint_identity(&runtime, &SigningKey::from_bytes(&[9u8; 32]))
                .unwrap_err()
                .to_string()
                .contains("must match")
        );
    }

    #[test]
    fn claim_trace_storage_caps_are_mandatory_and_bounded_per_purpose() {
        fn parse(
            input: &str,
            records_per_minute: u32,
        ) -> Result<(u64, u64), Box<dyn std::error::Error>> {
            let trace: RegistryClaimTraceFile = toml::from_str(input)?;
            let network = resolve_trace_storage_gib(
                "compliance.claim_trace.network_max_storage_gb",
                trace.network_max_storage_gb,
            )?;
            let identity = resolve_trace_storage_gib(
                "compliance.claim_trace.identity_max_storage_gb",
                trace.identity_max_storage_gb,
            )?;
            let capacity_epochs = trace.policy.capacity_epochs()?;
            let network_required = required_network_trace_storage_bytes(
                records_per_minute,
                capacity_epochs,
                trace.max_records_per_segment,
            )?;
            let identity_required = required_identity_trace_storage_bytes(
                records_per_minute,
                capacity_epochs,
                trace.max_records_per_segment,
            )?;
            validate_trace_storage_requirement(
                "compliance.claim_trace.network_max_storage_gb",
                network,
                network_required,
            )?;
            validate_trace_storage_requirement(
                "compliance.claim_trace.identity_max_storage_gb",
                identity,
                identity_required,
            )?;
            Ok((network, identity))
        }

        let fixture = |network: Option<u64>, identity: Option<u64>| {
            format!(
                r#"network_directory = "network"
identity_directory = "identity"
network_signing_key_file = "network.key"
identity_signing_key_file = "identity.key"
max_records_per_segment = 64
{}
{}

[policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 730
"#,
                network
                    .map(|value| format!("network_max_storage_gb = {value}"))
                    .unwrap_or_default(),
                identity
                    .map(|value| format!("identity_max_storage_gb = {value}"))
                    .unwrap_or_default()
            )
        };

        for missing in [fixture(None, Some(1)), fixture(Some(1), None)] {
            let error = parse(&missing, 1).unwrap_err().to_string();
            assert!(error.contains("max_storage_gb"));
        }

        let too_small = parse(&fixture(Some(0), Some(1)), 1)
            .unwrap_err()
            .to_string();
        assert!(too_small.contains("network_max_storage_gb"));

        let too_large = parse(&fixture(Some(1), Some(1_048_577)), 1)
            .unwrap_err()
            .to_string();
        assert!(too_large.contains("identity_max_storage_gb"));

        const GIB_BYTES: u64 = 1024 * 1024 * 1024;
        const RATE: u32 = 100;
        let network_required = required_network_trace_storage_bytes(RATE, 731, 64).unwrap();
        let network_gib = network_required.div_ceil(GIB_BYTES);
        let network_short = parse(&fixture(Some(network_gib - 1), Some(10_000)), RATE)
            .unwrap_err()
            .to_string();
        assert!(network_short.contains("network_max_storage_gb"));
        assert!(parse(&fixture(Some(network_gib), Some(10_000)), RATE).is_ok());

        let identity_required = required_identity_trace_storage_bytes(RATE, 731, 64).unwrap();
        let identity_gib = identity_required.div_ceil(GIB_BYTES);
        let identity_short = parse(&fixture(Some(10_000), Some(identity_gib - 1)), RATE)
            .unwrap_err()
            .to_string();
        assert!(identity_short.contains("identity_max_storage_gb"));
        assert!(parse(&fixture(Some(10_000), Some(identity_gib)), RATE).is_ok());
    }

    #[test]
    fn directory_publisher_pins_are_canonical_bounded_and_strong() {
        assert_eq!(resolve_directory_publishers(&[key(4)]).unwrap().len(), 1);
        assert!(resolve_directory_publishers(&[key(4), key(4)]).is_err());
        assert!(resolve_directory_publishers(&["AA".repeat(32)]).is_err());
        assert!(resolve_directory_publishers(&[format!("01{}", "00".repeat(31))]).is_err());
        assert!(resolve_directory_publishers(&vec![key(4); 65]).is_err());
    }

    #[test]
    fn unsafe_server_limits_are_rejected_by_the_runtime_builder() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("registry.toml"),
            "[server.limits]\nmax_concurrent_requests = 0\n",
        )
        .unwrap();
        let error = resolve_runtime(
            dir.path(),
            "127.0.0.1:7718",
            "registry.example/log",
            &ProviderSettings::default(),
            false,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("configuration") || error.contains("limit"));

        crate::test_support::write_private(
            dir.path().join("registry.toml"),
            "[server.limits]\naccount_bindings_per_minute = 0\n",
        )
        .unwrap();
        let error = resolve_runtime(
            dir.path(),
            "127.0.0.1:7718",
            "registry.example/log",
            &ProviderSettings::default(),
            false,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("account limits"));

        for response_limit in [0, MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE + 1] {
            crate::test_support::write_private(
                dir.path().join("registry.toml"),
                format!("[server.limits]\nglobal_response_bytes_per_minute = {response_limit}\n"),
            )
            .unwrap();
            let error = resolve_runtime(
                dir.path(),
                "127.0.0.1:7718",
                "registry.example/log",
                &ProviderSettings::default(),
                false,
            )
            .err()
            .unwrap()
            .to_string();
            assert!(
                error.contains("configuration") || error.contains("limit"),
                "{error}"
            );
        }
    }

    #[test]
    fn binding_ceiling_is_derived_without_reducing_read_only_http_capacity() {
        let configured: RegistryLimitsFile = toml::from_str(
            "global_requests_per_minute = 10000000\naccount_bindings_per_minute = 7\n",
        )
        .unwrap();
        let (http, registration) = configured.resolve().unwrap();
        assert_eq!(
            http.global_requests_per_minute,
            MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE
        );
        assert_eq!(
            registration.global_bindings_per_minute,
            MAX_REGISTRATION_BINDINGS_PER_MINUTE
        );
        assert_eq!(registration.account_bindings_per_minute, 7);
    }

    #[test]
    fn compliance_epoch_validation_uses_utc_days_and_calendar_months() {
        const FEBRUARY_2024: u64 = 1_706_745_600_000;
        const MARCH_2024: u64 = 1_709_251_200_000;
        let february = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Eu,
            [8; 32],
            FEBRUARY_2024,
            1,
        );
        assert_eq!(
            pigeonpost_compliance_format::attribution_epoch_end_ms(&february),
            Ok(MARCH_2024)
        );

        let custody = Identity::from_seed([7; 32]);
        let valid = ComplianceKeyPublish {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Eu,
                [8; 32],
                FEBRUARY_2024,
                1,
            ),
            public_key: hex(&keys::x25519_public(&custody)),
            not_before_ms: FEBRUARY_2024,
            not_after_ms: MARCH_2024,
            status: ComplianceKeyStatus::Active,
        };
        validate_operator_publication(&valid).unwrap();

        let mut fixed_31_days = valid;
        fixed_31_days.not_after_ms = fixed_31_days.not_before_ms + 31 * DAY_MS;
        assert!(validate_operator_publication(&fixed_31_days).is_err());
    }

    #[test]
    fn operator_rejects_test_ids_uppercase_hex_and_low_order_x25519_points() {
        let test_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [9; 32],
            0,
            1,
        );
        let key_id = hex(&test_id.encode().unwrap());
        let options = ComplianceOperatorOptions {
            dir: PathBuf::from("."),
            origin: "registry.example/log".into(),
            key_id: key_id.clone(),
            confirm_key_id: key_id,
            checkpoint_backup: PathBuf::from("/unused"),
            witness_timeout_seconds: 1,
            confirm_offline: false,
            execute: false,
            json: true,
        };
        assert!(validate_common_operator_options(&options).is_err());
        assert!(decode_lower_hex::<32>(&"AA".repeat(32), "bad").is_err());

        let low_order = ComplianceKeyPublish {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Tr,
                [9; 32],
                0,
                1,
            ),
            public_key: "00".repeat(32),
            not_before_ms: 0,
            not_after_ms: DAY_MS,
            status: ComplianceKeyStatus::Active,
        };
        assert!(validate_operator_publication(&low_order).is_err());
    }

    #[test]
    fn checkpoint_backup_must_be_an_independent_copy_outside_the_registry_dir() {
        let root = crate::test_support::private_tempdir();
        let dir = root.path().join("registry");
        let _registry_dir = PrivateDirectory::open_or_create(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let live = dir.join("checkpoint.key");
        let backup = root.path().join("checkpoint-backup.key");
        crate::test_support::write_private(&live, [7; 32]).unwrap();
        crate::test_support::write_private(&backup, [7; 32]).unwrap();
        #[cfg(unix)]
        for path in [&live, &backup] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        validate_checkpoint_backup(&dir, &backup).unwrap();

        let inside = dir.join("not-a-backup.key");
        crate::test_support::write_private(&inside, [7; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(validate_checkpoint_backup(&dir, &inside).is_err());
        crate::test_support::write_private(&backup, [8; 32]).unwrap();
        assert!(validate_checkpoint_backup(&dir, &backup).is_err());
    }

    #[test]
    fn registry_server_and_operator_share_one_nonblocking_process_lock() {
        let dir = crate::test_support::private_tempdir();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let first = RegistryProcessLock::acquire(dir.path()).unwrap();
        let error = match RegistryProcessLock::acquire(dir.path()) {
            Ok(_) => panic!("a second registry process lock unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already running"));

        drop(first);
        assert!(RegistryProcessLock::acquire(dir.path()).is_ok());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let root = crate::test_support::private_tempdir();
            let link = root.path().join("registry-link");
            symlink(dir.path(), &link).unwrap();
            assert!(RegistryProcessLock::acquire(&link).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn registry_lock_rejects_unsafe_namespaces_without_side_effects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = crate::test_support::private_tempdir();
        let mutable = root.path().join("mutable");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(RegistryProcessLock::acquire(&mutable).is_err());
        assert!(!mutable.join("registry.lock").exists());
        assert_eq!(
            std::fs::metadata(&mutable).unwrap().permissions().mode() & 0o7777,
            0o770
        );

        let private = root.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = private.join("target");
        crate::test_support::write_private(&target, b"sentinel").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, private.join("registry.lock")).unwrap();
        assert!(RegistryProcessLock::acquire(&private).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[tokio::test]
    async fn unsupported_registry_persistence_fails_before_any_storage_mutation() {
        let root = crate::test_support::private_tempdir();
        let storage = root.path().join("registry");
        let error = serve("127.0.0.1:7718", &storage, "registry.test/no-storage", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("verified Linux or macOS storage custody"));
        assert!(!storage.exists());

        let options = ComplianceOperatorOptions {
            dir: storage.clone(),
            origin: "registry.test/no-storage".into(),
            key_id: String::new(),
            confirm_key_id: String::new(),
            checkpoint_backup: root.path().join("missing-backup"),
            witness_timeout_seconds: 1,
            confirm_offline: true,
            execute: true,
            json: false,
        };
        assert!(open_compliance_operator(&options).is_err());
        assert!(!storage.exists());
        assert!(!options.checkpoint_backup.exists());
    }
}
