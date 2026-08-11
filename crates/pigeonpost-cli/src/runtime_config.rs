//! Strict operator configuration shared by the registry and loft startup paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pigeonpost_compliance_format::Jurisdiction;
use pigeonpost_compliance_seal::{
    MAX_SEGMENT_RECORDS, MAX_TRACE_STORAGE_BYTES, MIN_TRACE_STORAGE_BYTES,
};
use pigeonpost_core::{
    keys,
    network::{is_localhost_name, is_numeric_loopback_host},
};
use pigeonpost_directory::private_store::{
    load_secret32, read_private_file_bounded, read_trusted_file_bounded,
};
use pigeonpost_loft::{CapturePolicy, CheckpointPin, WitnessKeyConfig, WitnessedRegistryConfig};
use pigeonpost_registry::claim_trace::ClaimCapturePolicy;
use pigeonpost_registry::{
    witness_quorum_intersects, WitnessClient, WitnessConfig, WitnessPolicy, WitnessTiming,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use zeroize::Zeroizing;

pub const MAX_OPERATOR_CONFIG_BYTES: u64 = 64 * 1024;
pub const MAX_PROVIDER_SECRET_BYTES: u64 = 4 * 1024;
const GIB_BYTES: u64 = 1024 * 1024 * 1024;
const UTC_DAY_MS: u64 = 86_400_000;
const US_TRACE_RETENTION_DAYS: u64 = 30;
const TR_MIN_TRACE_RETENTION_DAYS: u64 = 365;
const TR_MAX_TRACE_RETENTION_DAYS: u64 = 730;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessedRegistryFile {
    pub registry_url: String,
    pub expected_origin: String,
    pub registry_checkpoint_key: String,
    pub witnesses: Vec<WitnessFile>,
    pub witness_threshold: usize,
    pub minimum_checkpoint_size: u64,
    pub minimum_checkpoint_root: String,
    pub max_staleness_seconds: u64,
    pub refresh_interval_seconds: u64,
    pub state_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessFile {
    pub name: String,
    pub public_key: String,
}

/// Registry-operator witness submission policy. This is intentionally separate from
/// `compliance.registry`: the latter is a read-side trust policy, while this block supplies the
/// independently operated C2SP endpoints to which the registry submits its own checkpoints.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryWitnessingFile {
    pub threshold: usize,
    pub max_cosignature_age_seconds: u64,
    #[serde(default = "default_future_clock_skew_seconds")]
    pub future_clock_skew_seconds: u64,
    #[serde(default)]
    pub max_lag_entries: u64,
    #[serde(default = "default_witness_poll_seconds")]
    pub poll_interval_seconds: u64,
    #[serde(default = "default_witness_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_witness_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_witness_retry_initial_ms")]
    pub retry_initial_ms: u64,
    #[serde(default = "default_witness_retry_max_ms")]
    pub retry_max_ms: u64,
    #[serde(default = "default_witness_retry_deadline_seconds")]
    pub retry_deadline_seconds: u64,
    pub witnesses: Vec<WitnessOperatorFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessOperatorFile {
    pub name: String,
    pub public_key: String,
    pub submission_prefix: String,
    pub monitoring_prefix: String,
}

pub struct ResolvedRegistryWitnessing {
    pub policy: WitnessPolicy,
    pub clients: Vec<WitnessClient>,
    pub poll_interval: Duration,
    pub failure_backoff_initial: Duration,
    pub failure_backoff_max: Duration,
}

impl RegistryWitnessingFile {
    pub fn resolve(
        &self,
        origin: &str,
        operator_key: ed25519_dalek::VerifyingKey,
    ) -> Result<ResolvedRegistryWitnessing, Box<dyn std::error::Error>> {
        if self.max_lag_entries > 1_000_000
            || !witness_quorum_intersects(self.threshold, self.witnesses.len())
        {
            return Err("registry witnessing configuration is incomplete or unsafe".into());
        }
        let timing = WitnessTiming {
            connect_timeout: Duration::from_secs(self.connect_timeout_seconds),
            request_timeout: Duration::from_secs(self.request_timeout_seconds),
            max_cosignature_age: Duration::from_secs(self.max_cosignature_age_seconds),
            future_clock_skew: Duration::from_secs(self.future_clock_skew_seconds),
            retry_initial: Duration::from_millis(self.retry_initial_ms),
            retry_max: Duration::from_millis(self.retry_max_ms),
            retry_deadline: Duration::from_secs(self.retry_deadline_seconds),
        };
        let mut configs = Vec::with_capacity(self.witnesses.len());
        let mut witness_keys = Vec::with_capacity(self.witnesses.len());
        for witness in &self.witnesses {
            let public_key = parse_hex32(
                &witness.public_key,
                "witness public_key must be exactly 32 lowercase hex bytes",
            )?;
            let public_key = keys::verifying_key_from_bytes(&public_key)
                .map_err(|_| "witness public_key is not a valid Ed25519 public key")?;
            if public_key.as_bytes() == operator_key.as_bytes() {
                return Err(
                    "witness keys must be distinct from the registry checkpoint key".into(),
                );
            }
            let config = WitnessConfig::new(
                witness.name.clone(),
                public_key,
                &witness.submission_prefix,
                &witness.monitoring_prefix,
                origin,
            )?;
            witness_keys.push(config.witness_key());
            configs.push(config);
        }
        let policy = WitnessPolicy::new(
            witness_keys,
            self.threshold,
            self.max_cosignature_age_seconds,
            self.future_clock_skew_seconds,
            self.max_lag_entries,
        )?;
        let clients = configs
            .into_iter()
            .map(|config| WitnessClient::new(config, origin, operator_key, timing))
            .collect::<Result<Vec<_>, _>>()?;
        let poll_interval = Duration::from_secs(self.poll_interval_seconds);
        if poll_interval.is_zero() || poll_interval > Duration::from_secs(5 * 60) {
            return Err("witness poll interval must be between one and 300 seconds".into());
        }
        Ok(ResolvedRegistryWitnessing {
            policy,
            clients,
            poll_interval,
            failure_backoff_initial: timing.retry_initial,
            failure_backoff_max: timing.retry_max,
        })
    }
}

const fn default_future_clock_skew_seconds() -> u64 {
    30
}

const fn default_witness_poll_seconds() -> u64 {
    5
}

const fn default_witness_connect_timeout_seconds() -> u64 {
    5
}

const fn default_witness_request_timeout_seconds() -> u64 {
    15
}

const fn default_witness_retry_initial_ms() -> u64 {
    250
}

const fn default_witness_retry_max_ms() -> u64 {
    15_000
}

const fn default_witness_retry_deadline_seconds() -> u64 {
    60
}

impl WitnessedRegistryFile {
    pub fn resolve(
        &self,
        base: &Path,
    ) -> Result<WitnessedRegistryConfig, Box<dyn std::error::Error>> {
        let checkpoint_key = parse_hex32(
            &self.registry_checkpoint_key,
            "registry_checkpoint_key must be exactly 32 lowercase hex bytes",
        )?;
        if self.expected_origin.is_empty()
            || self.expected_origin.len() > 256
            || self
                .expected_origin
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || checkpoint_key == [0u8; 32]
            || self.witnesses.is_empty()
            || !witness_quorum_intersects(self.witness_threshold, self.witnesses.len())
            || self.max_staleness_seconds == 0
            || self.refresh_interval_seconds == 0
            || self.refresh_interval_seconds >= self.max_staleness_seconds
            || self.state_path.as_os_str().is_empty()
        {
            return Err("witnessed registry configuration is incomplete or unsafe".into());
        }
        keys::verifying_key_from_bytes(&checkpoint_key)
            .map_err(|_| "registry checkpoint key is not a valid Ed25519 public key")?;
        validate_registry_url(&self.registry_url)?;
        let minimum_root = parse_hex32(
            &self.minimum_checkpoint_root,
            "minimum_checkpoint_root must be exactly 32 lowercase hex bytes",
        )?;
        let empty_root = pigeonpost_registry::log::empty_root();
        if (self.minimum_checkpoint_size == 0 && minimum_root != empty_root)
            || (self.minimum_checkpoint_size != 0 && minimum_root == empty_root)
        {
            return Err(
                "a zero-size minimum checkpoint must use the RFC 6962 empty-tree root, and a nonzero checkpoint must not"
                    .into(),
            );
        }

        let mut names = HashSet::new();
        let mut keys = HashSet::new();
        let mut witnesses = Vec::with_capacity(self.witnesses.len());
        for witness in &self.witnesses {
            if witness.name.is_empty()
                || witness.name.len() > 128
                || witness.name.bytes().any(|byte| byte.is_ascii_control())
                || !names.insert(witness.name.clone())
            {
                return Err("witness names must be nonempty, bounded, and unique".into());
            }
            let public_key = parse_hex32(
                &witness.public_key,
                "witness public_key must be exactly 32 lowercase hex bytes",
            )?;
            if public_key == checkpoint_key || !keys.insert(public_key) {
                return Err(
                    "witness keys must be unique and distinct from the registry key".into(),
                );
            }
            keys::verifying_key_from_bytes(&public_key)
                .map_err(|_| "witness public_key is not a valid Ed25519 public key")?;
            witnesses.push(WitnessKeyConfig {
                name: witness.name.clone(),
                public_key,
            });
        }

        let max_staleness_ms = self
            .max_staleness_seconds
            .checked_mul(1_000)
            .ok_or("max_staleness_seconds is too large")?;
        let refresh_interval_ms = self
            .refresh_interval_seconds
            .checked_mul(1_000)
            .ok_or("refresh_interval_seconds is too large")?;

        Ok(WitnessedRegistryConfig {
            registry_url: self.registry_url.clone(),
            expected_origin: self.expected_origin.clone(),
            registry_checkpoint_key: checkpoint_key,
            witnesses,
            witness_threshold: self.witness_threshold,
            minimum_checkpoint: CheckpointPin {
                size: self.minimum_checkpoint_size,
                root: minimum_root,
            },
            max_staleness_ms,
            refresh_interval_ms,
            state_path: resolve_path(base, &self.state_path)?,
        })
    }
}

pub fn validate_segment_limit(value: u32) -> Result<(), Box<dyn std::error::Error>> {
    if value == 0 || value > MAX_SEGMENT_RECORDS {
        return Err(
            format!("max_records_per_segment must be between 1 and {MAX_SEGMENT_RECORDS}").into(),
        );
    }
    Ok(())
}

/// Convert an operator-supplied whole-GiB trace cap without permitting overflow or a value the
/// sealed writer cannot enforce.
pub fn resolve_trace_storage_gib(
    field: &'static str,
    gib: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let bytes = gib
        .checked_mul(GIB_BYTES)
        .ok_or_else(|| format!("{field} overflows the byte-sized trace storage cap"))?;
    if !(MIN_TRACE_STORAGE_BYTES..=MAX_TRACE_STORAGE_BYTES).contains(&bytes) {
        return Err(format!(
            "{field} must resolve to between {MIN_TRACE_STORAGE_BYTES} and {MAX_TRACE_STORAGE_BYTES} bytes"
        )
        .into());
    }
    Ok(bytes)
}

pub fn validate_trace_storage_requirement(
    field: &'static str,
    configured_bytes: u64,
    required_bytes: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    if configured_bytes < required_bytes {
        return Err(format!(
            "{field} provides {configured_bytes} bytes but requires at least {required_bytes} bytes for the configured admission rate and capacity runway"
        )
        .into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Standing,
    Preservation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracePolicyFile {
    pub jurisdiction: Jurisdiction,
    pub capture: CaptureMode,
    pub retention_days: Option<u64>,
    pub preservation_starts_at_ms: Option<u64>,
    pub preservation_expires_at_ms: Option<u64>,
}

impl TracePolicyFile {
    pub fn effective_retention_days(&self) -> Result<Option<u64>, Box<dyn std::error::Error>> {
        match self.checked()? {
            CheckedCapture::Standing { retention_days } => Ok(Some(retention_days)),
            CheckedCapture::Preservation { .. } => Ok(None),
        }
    }

    pub fn loft_policy(&self) -> Result<CapturePolicy, Box<dyn std::error::Error>> {
        match self.checked()? {
            CheckedCapture::Standing { .. } => Ok(CapturePolicy::Standing),
            CheckedCapture::Preservation {
                starts_at_ms,
                expires_at_ms,
            } => Ok(CapturePolicy::Preservation {
                starts_at_ms,
                expires_at_ms,
            }),
        }
    }

    pub fn registry_policy(&self) -> Result<ClaimCapturePolicy, Box<dyn std::error::Error>> {
        match self.checked()? {
            CheckedCapture::Standing { .. } => Ok(ClaimCapturePolicy::Standing),
            CheckedCapture::Preservation {
                starts_at_ms,
                expires_at_ms,
            } => Ok(ClaimCapturePolicy::Preservation {
                starts_at_ms,
                expires_at_ms,
            }),
        }
    }

    /// Count UTC epochs that the append-only logical budget must be able to carry. Standing
    /// policies include the current open epoch in addition to retained closed history; EU counts
    /// every UTC epoch intersected by its half-open preservation interval. This is capacity
    /// runway, not a filesystem quota, deletion schedule, or legal-retention mechanism.
    pub fn capacity_epochs(&self) -> Result<u64, Box<dyn std::error::Error>> {
        match self.checked()? {
            CheckedCapture::Standing { retention_days } => retention_days
                .checked_add(1)
                .ok_or_else(|| "standing trace capacity interval is too large".into()),
            CheckedCapture::Preservation {
                starts_at_ms,
                expires_at_ms,
            } => {
                let first_epoch = starts_at_ms / UTC_DAY_MS;
                let last_epoch = (expires_at_ms - 1) / UTC_DAY_MS;
                last_epoch
                    .checked_sub(first_epoch)
                    .and_then(|epochs| epochs.checked_add(1))
                    .ok_or_else(|| "EU preservation UTC-epoch interval is invalid".into())
            }
        }
    }

    fn checked(&self) -> Result<CheckedCapture, Box<dyn std::error::Error>> {
        match (
            self.jurisdiction,
            self.capture,
            self.retention_days,
            self.preservation_starts_at_ms,
            self.preservation_expires_at_ms,
        ) {
            (
                Jurisdiction::Eu,
                CaptureMode::Preservation,
                None,
                Some(starts_at_ms),
                Some(expires_at_ms),
            ) if starts_at_ms > 0 && starts_at_ms < expires_at_ms => {
                Ok(CheckedCapture::Preservation {
                    starts_at_ms,
                    expires_at_ms,
                })
            }
            (
                Jurisdiction::Us,
                CaptureMode::Standing,
                retention_days,
                None,
                None,
            ) if retention_days.is_none_or(|days| days == US_TRACE_RETENTION_DAYS) => {
                Ok(CheckedCapture::Standing {
                    retention_days: US_TRACE_RETENTION_DAYS,
                })
            }
            (
                Jurisdiction::Tr,
                CaptureMode::Standing,
                Some(retention_days),
                None,
                None,
            ) if (TR_MIN_TRACE_RETENTION_DAYS..=TR_MAX_TRACE_RETENTION_DAYS)
                .contains(&retention_days) =>
            {
                Ok(CheckedCapture::Standing { retention_days })
            }
            _ => Err(
                "EU trace capture requires only a bounded preservation interval; US standing capture is fixed at 30 retention days; TR standing capture requires retention_days between 365 and 730; the test jurisdiction is not accepted by the operator CLI"
                    .into(),
            ),
        }
    }
}

pub fn validate_separate_directories(
    first: &Path,
    second: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if first == second || first.starts_with(second) || second.starts_with(first) {
        return Err(
            "network and identity trace directories must be separate and non-nested".into(),
        );
    }
    Ok(())
}

enum CheckedCapture {
    Standing {
        retention_days: u64,
    },
    Preservation {
        starts_at_ms: u64,
        expires_at_ms: u64,
    },
}

pub fn load_optional_toml<T: DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, Box<dyn std::error::Error>> {
    let bytes = match read_trusted_file_bounded(path, MAX_OPERATOR_CONFIG_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("invalid {}: configuration is not UTF-8", path.display()))?;
    let value =
        toml::from_str(text).map_err(|error| format!("invalid {}: {error}", path.display()))?;
    Ok(Some(value))
}

pub fn read_existing_seed(
    base: &Path,
    configured_path: &Path,
) -> Result<Zeroizing<[u8; 32]>, Box<dyn std::error::Error>> {
    let path = resolve_path(base, configured_path)?;
    load_secret32(&path)
        .map_err(|error| format!("cannot load private seed {}: {error}", path.display()).into())
}

/// Read one long-lived provider credential without accepting ambient filesystem authority.
///
/// Production callers pass an absolute path mounted read-only into the service. The opened
/// descriptor, rather than a second path lookup, supplies the bytes after link, identity,
/// ownership, permission, kind, and size validation.
pub fn read_owner_only_secret(
    path: &Path,
) -> Result<Zeroizing<String>, Box<dyn std::error::Error>> {
    let bytes = read_private_file_bounded(path, MAX_PROVIDER_SECRET_BYTES)
        .map_err(|error| format!("cannot read provider secret safely: {error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_PROVIDER_SECRET_BYTES {
        return Err("provider secret must be nonempty and no larger than 4 KiB".into());
    }
    if bytes.iter().any(|byte| !(0x21..=0x7e).contains(byte)) {
        return Err(
            "provider secret must contain one line of visible ASCII without whitespace".into(),
        );
    }
    let secret = std::str::from_utf8(&bytes)
        .map_err(|_| "provider secret must contain valid visible ASCII")?;
    Ok(Zeroizing::new(secret.to_owned()))
}

pub fn resolve_path(base: &Path, path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if path.as_os_str().is_empty()
        || (!path.is_absolute()
            && path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            }))
    {
        return Err(
            "configured paths must be absolute or relative without parent traversal".into(),
        );
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(base.join(path))
    }
}

fn parse_hex32(input: &str, message: &'static str) -> Result<[u8; 32], &'static str> {
    if input.len() != 64
        || input
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(message);
    }
    let mut output = [0u8; 32];
    for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
        output[index] = u8::from_str_radix(std::str::from_utf8(chunk).map_err(|_| message)?, 16)
            .map_err(|_| message)?;
    }
    Ok(output)
}

fn validate_registry_url(input: &str) -> Result<(), Box<dyn std::error::Error>> {
    if input.is_empty() || input.len() > 2_048 {
        return Err("registry_url is empty or too long".into());
    }
    let url = reqwest::Url::parse(input).map_err(|_| "registry_url is not a valid URL")?;
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || url.host_str().is_some_and(is_localhost_name)
        || url.port() == Some(0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        return Err("registry_url must not contain credentials, a query, or a fragment".into());
    }
    let local = url.host_str().is_some_and(is_numeric_loopback_host);
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return Err(
            "registry_url must use HTTPS (HTTP is accepted only for loopback tests)".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;

    fn key(seed: u8) -> String {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn registry() -> WitnessedRegistryFile {
        WitnessedRegistryFile {
            registry_url: "https://registry.example/".into(),
            expected_origin: "registry.example/log".into(),
            registry_checkpoint_key: key(1),
            witnesses: vec![
                WitnessFile {
                    name: "one".into(),
                    public_key: key(2),
                },
                WitnessFile {
                    name: "two".into(),
                    public_key: key(3),
                },
            ],
            witness_threshold: 2,
            minimum_checkpoint_size: 1,
            minimum_checkpoint_root: "44".repeat(32),
            max_staleness_seconds: 600,
            refresh_interval_seconds: 60,
            state_path: "state/registry.json".into(),
        }
    }

    #[test]
    fn witnessed_registry_paths_and_time_budgets_resolve_from_the_operator_directory() {
        let base = Path::new("/srv/pigeonpost");
        let resolved = registry().resolve(base).unwrap();
        assert_eq!(resolved.witness_threshold, 2);
        assert_eq!(resolved.max_staleness_ms, 600_000);
        assert_eq!(resolved.refresh_interval_ms, 60_000);
        assert_eq!(
            resolved.state_path,
            Path::new("/srv/pigeonpost/state/registry.json")
        );
    }

    #[test]
    fn witnessed_registry_requires_a_strictly_intersecting_quorum() {
        let mut configuration = registry();
        configuration.witness_threshold = 1;
        assert!(configuration.resolve(Path::new(".")).is_err());

        configuration.witnesses.push(WitnessFile {
            name: "three".into(),
            public_key: key(4),
        });
        assert!(configuration.resolve(Path::new(".")).is_err());
        configuration.witness_threshold = 2;
        assert!(configuration.resolve(Path::new(".")).is_ok());
    }

    fn witnessing(count: usize, threshold: usize) -> RegistryWitnessingFile {
        RegistryWitnessingFile {
            threshold,
            max_cosignature_age_seconds: 600,
            future_clock_skew_seconds: 30,
            max_lag_entries: 0,
            poll_interval_seconds: 5,
            connect_timeout_seconds: 5,
            request_timeout_seconds: 15,
            retry_initial_ms: 250,
            retry_max_ms: 15_000,
            retry_deadline_seconds: 60,
            witnesses: (0..count)
                .map(|index| WitnessOperatorFile {
                    name: format!("witness-{index}"),
                    public_key: key(u8::try_from(index + 2).unwrap()),
                    submission_prefix: format!("https://witness-{index}.example/submission"),
                    monitoring_prefix: format!("https://witness-{index}.example/monitoring"),
                })
                .collect(),
        }
    }

    #[test]
    fn registry_witness_submission_requires_a_strictly_intersecting_quorum() {
        let operator = ed25519_dalek::SigningKey::from_bytes(&[1; 32]).verifying_key();
        assert!(witnessing(1, 1)
            .resolve("registry.example/log", operator)
            .is_ok());
        assert!(witnessing(2, 1)
            .resolve("registry.example/log", operator)
            .is_err());
        assert!(witnessing(3, 1)
            .resolve("registry.example/log", operator)
            .is_err());
        assert!(witnessing(3, 2)
            .resolve("registry.example/log", operator)
            .is_ok());
    }

    #[test]
    fn default_witness_poll_fits_the_directory_publication_wait() {
        assert_eq!(default_witness_poll_seconds(), 5);
    }

    #[test]
    fn trace_storage_gib_conversion_is_checked_and_uses_writer_bounds() {
        assert_eq!(
            resolve_trace_storage_gib("trace.max_storage_gb", 1).unwrap(),
            GIB_BYTES
        );
        assert!(resolve_trace_storage_gib("trace.max_storage_gb", 0).is_err());
        assert!(resolve_trace_storage_gib(
            "trace.max_storage_gb",
            MAX_TRACE_STORAGE_BYTES / GIB_BYTES + 1,
        )
        .is_err());
        let overflow = resolve_trace_storage_gib("trace.max_storage_gb", u64::MAX)
            .unwrap_err()
            .to_string();
        assert!(overflow.contains("overflows"));
    }

    #[test]
    fn duplicate_or_registry_owned_witness_keys_are_rejected() {
        let mut duplicate = registry();
        duplicate.witnesses[1].public_key = duplicate.witnesses[0].public_key.clone();
        assert!(duplicate.resolve(Path::new(".")).is_err());

        let mut registry_owned = registry();
        registry_owned.witnesses[0].public_key = registry_owned.registry_checkpoint_key.clone();
        assert!(registry_owned.resolve(Path::new(".")).is_err());
    }

    #[test]
    fn registry_http_exception_is_confined_to_exact_loopback_hosts() {
        let mut ipv6 = registry();
        ipv6.registry_url = "http://[::1]:7718/".into();
        assert!(ipv6.resolve(Path::new(".")).is_ok());

        for unsafe_url in [
            "http://localhost:7718/",
            "http://localhost.:7718/",
            "https://localhost:7718/",
            "https://localhost.:7718/",
            "https://api.localhost:7718/",
            "https://registry.example:0/",
            "https://user@example.com/",
            "https://registry.example/prefix",
            "https://registry.example/?query=1",
        ] {
            let mut unsafe_registry = registry();
            unsafe_registry.registry_url = unsafe_url.into();
            assert!(
                unsafe_registry.resolve(Path::new(".")).is_err(),
                "accepted {unsafe_url}"
            );
        }

        let mut non_loopback = registry();
        non_loopback.registry_url = "http://192.0.2.1:7718/".into();
        assert!(non_loopback.resolve(Path::new(".")).is_err());
    }

    #[test]
    fn zero_size_pin_requires_the_rfc6962_empty_tree_root() {
        let mut zero = registry();
        zero.minimum_checkpoint_size = 0;
        zero.minimum_checkpoint_root = pigeonpost_registry::log::empty_root()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(zero.resolve(Path::new(".")).is_ok());

        zero.minimum_checkpoint_root = "00".repeat(32);
        assert!(zero.resolve(Path::new(".")).is_err());
    }

    #[test]
    fn jurisdiction_and_capture_mode_are_a_fail_closed_pair() {
        let eu = TracePolicyFile {
            jurisdiction: Jurisdiction::Eu,
            capture: CaptureMode::Preservation,
            retention_days: None,
            preservation_starts_at_ms: Some(10),
            preservation_expires_at_ms: Some(20),
        };
        assert!(matches!(
            eu.loft_policy().unwrap(),
            CapturePolicy::Preservation { .. }
        ));

        let invalid = TracePolicyFile {
            jurisdiction: Jurisdiction::Eu,
            capture: CaptureMode::Standing,
            retention_days: None,
            preservation_starts_at_ms: None,
            preservation_expires_at_ms: None,
        };
        assert!(invalid.registry_policy().is_err());
    }

    #[test]
    fn trace_capacity_epochs_include_the_open_epoch_and_each_intersected_utc_day() {
        let standing = |jurisdiction, retention_days| TracePolicyFile {
            jurisdiction,
            capture: CaptureMode::Standing,
            retention_days,
            preservation_starts_at_ms: None,
            preservation_expires_at_ms: None,
        };
        assert_eq!(
            standing(Jurisdiction::Us, None).capacity_epochs().unwrap(),
            31
        );
        assert_eq!(
            standing(Jurisdiction::Us, Some(30))
                .capacity_epochs()
                .unwrap(),
            31
        );
        assert!(standing(Jurisdiction::Us, Some(31))
            .capacity_epochs()
            .is_err());
        assert_eq!(
            standing(Jurisdiction::Tr, Some(365))
                .capacity_epochs()
                .unwrap(),
            366
        );
        assert_eq!(
            standing(Jurisdiction::Tr, Some(730))
                .capacity_epochs()
                .unwrap(),
            731
        );
        for retention_days in [None, Some(364), Some(731)] {
            assert!(standing(Jurisdiction::Tr, retention_days)
                .capacity_epochs()
                .is_err());
        }

        let preservation = |starts_at_ms, expires_at_ms| TracePolicyFile {
            jurisdiction: Jurisdiction::Eu,
            capture: CaptureMode::Preservation,
            retention_days: None,
            preservation_starts_at_ms: Some(starts_at_ms),
            preservation_expires_at_ms: Some(expires_at_ms),
        };
        assert_eq!(preservation(1, 2).capacity_epochs().unwrap(), 1);
        assert_eq!(
            preservation(UTC_DAY_MS, UTC_DAY_MS + 1)
                .capacity_epochs()
                .unwrap(),
            1
        );
        assert_eq!(
            preservation(UTC_DAY_MS - 1, UTC_DAY_MS + 1)
                .capacity_epochs()
                .unwrap(),
            2
        );
        assert_eq!(
            preservation(1, UTC_DAY_MS + 1).capacity_epochs().unwrap(),
            2
        );

        let mut invalid_eu = preservation(1, 2);
        invalid_eu.retention_days = Some(1);
        assert!(invalid_eu.capacity_epochs().is_err());
    }

    #[test]
    fn workload_requirement_accepts_the_exact_boundary_only() {
        assert!(validate_trace_storage_requirement("trace.max_storage_gb", 99, 100).is_err());
        assert!(validate_trace_storage_requirement("trace.max_storage_gb", 100, 100).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn existing_signing_seeds_must_be_owner_only_regular_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_support::private_tempdir();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("trace.key");
        fs::write(&path, [7u8; 32]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_existing_seed(dir.path(), Path::new("trace.key")).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            *read_existing_seed(dir.path(), Path::new("trace.key")).unwrap(),
            [7u8; 32]
        );
    }

    #[cfg(unix)]
    #[test]
    fn optional_toml_uses_a_bounded_trusted_descriptor_walk() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        #[derive(Debug, Deserialize, PartialEq)]
        struct Example {
            value: u8,
        }

        let root = crate::test_support::private_tempdir();
        let config_dir = root.path().join("config");
        fs::create_dir(&config_dir).unwrap();
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let config = config_dir.join("operator.toml");
        fs::write(&config, b"value = 7\n").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            load_optional_toml::<Example>(&config).unwrap(),
            Some(Example { value: 7 })
        );

        fs::write(&config, vec![b' '; MAX_OPERATOR_CONFIG_BYTES as usize + 1]).unwrap();
        assert!(load_optional_toml::<Example>(&config).is_err());
        fs::write(&config, b"value = 7\n").unwrap();

        let linked = root.path().join("linked");
        symlink(&config_dir, &linked).unwrap();
        assert!(load_optional_toml::<Example>(&linked.join("operator.toml")).is_err());

        let mutable = root.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o777)).unwrap();
        let unsafe_config = mutable.join("operator.toml");
        fs::write(&unsafe_config, b"value = 7\n").unwrap();
        assert!(load_optional_toml::<Example>(&unsafe_config).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn provider_credentials_require_private_mode_single_link_and_regular_kind() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = crate::test_support::private_tempdir();
        let secrets = root.path().join("secrets");
        fs::create_dir(&secrets).unwrap();
        fs::set_permissions(&secrets, fs::Permissions::from_mode(0o755)).unwrap();
        let credential = secrets.join("provider");
        fs::write(&credential, b"visible-secret").unwrap();

        for mode in [0o400, 0o600] {
            fs::set_permissions(&credential, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                &*read_owner_only_secret(&credential).unwrap(),
                "visible-secret"
            );
        }
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o200)).unwrap();
        assert!(read_owner_only_secret(&credential).is_err());
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();

        let hardlink = secrets.join("provider-link");
        fs::hard_link(&credential, &hardlink).unwrap();
        assert!(read_owner_only_secret(&credential).is_err());
        fs::remove_file(&hardlink).unwrap();

        let linked = secrets.join("provider-symlink");
        symlink(&credential, &linked).unwrap();
        assert!(read_owner_only_secret(&linked).is_err());

        let fifo = secrets.join("provider-fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(read_owner_only_secret(&fifo).is_err());
    }
}
