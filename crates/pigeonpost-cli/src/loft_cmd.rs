//! `pigeonpost loft serve` — run a supervised node from its installed configuration.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use pigeonpost_compliance_format::Jurisdiction;
use pigeonpost_compliance_seal::required_network_trace_storage_bytes;
use serde::Deserialize;
use zeroize::Zeroizing;

use pigeonpost_loft::{
    AttributionKeyResolver, Loft, LoftConfig, LoftStore, SealedTraceConfig, SealedTraceSink,
    SqliteStore, TraceKeyResolver, TraceSegmentCatalog, TraceSink, WitnessedRegistryConfig,
    WitnessedRegistryKeyCache, MAX_CAPACITY_BYTES, MAX_RETENTION_DAYS, MAX_TRUSTED_PROXIES,
};

use crate::runtime_config::{
    load_optional_toml, read_existing_seed, resolve_path, resolve_trace_storage_gib,
    validate_segment_limit, validate_trace_storage_requirement, TracePolicyFile,
    WitnessedRegistryFile,
};

const DEFAULT_BIND: &str = "127.0.0.1:7717";
const DEFAULT_CAPACITY_GB: u64 = 20;
const DEFAULT_RETENTION_DAYS: u64 = 30;
const MAX_CAPACITY_GB: u64 = MAX_CAPACITY_BYTES / (1024 * 1024 * 1024);

pub struct ServeOptions {
    pub dir: PathBuf,
    pub bind: Option<String>,
    pub capacity_gb: Option<u64>,
    pub retention_days: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledConfig {
    loft: InstalledLoft,
    #[serde(default)]
    pool: InstalledPool,
    compliance: Option<LoftComplianceFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledLoft {
    bind: String,
    storage_path: PathBuf,
    capacity_gb: u64,
    retention_days: u64,
    #[serde(default)]
    trusted_proxies: Vec<IpAddr>,
    policy: Option<InstalledPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledPolicy {
    open: bool,
    pow_floor: u32,
    max_event_bytes: usize,
    #[serde(default)]
    allowlist: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledPool {
    #[serde(default)]
    join: bool,
    domain: Option<String>,
    directory_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoftComplianceFile {
    registry: WitnessedRegistryFile,
    trace: LoftTraceFile,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoftTraceFile {
    policy: TracePolicyFile,
    directory: PathBuf,
    signing_key_file: PathBuf,
    max_records_per_segment: u32,
    max_storage_gb: u64,
}

struct ResolvedLoftCompliance {
    registry: WitnessedRegistryConfig,
    jurisdiction: Jurisdiction,
    capture_policy: pigeonpost_loft::CapturePolicy,
    retention_days: Option<u64>,
    trace_directory: PathBuf,
    signing_key_file: PathBuf,
    max_records_per_segment: u32,
    max_storage_bytes: u64,
    capacity_epochs: u64,
}

struct ResolvedOptions {
    bind: String,
    origin: String,
    storage_path: PathBuf,
    capacity_gb: u64,
    retention_days: u64,
    max_event_bytes: Option<usize>,
    trusted_proxies: Vec<IpAddr>,
    compliance: Option<ResolvedLoftCompliance>,
}

pub async fn serve(options: ServeOptions) -> Result<(), Box<dyn std::error::Error>> {
    require_supported_loft_platform(cfg!(any(target_os = "linux", target_os = "macos", windows)))?;
    let resolved = resolve_options(&options)?;
    validate_platform_custody(
        &resolved,
        cfg!(any(target_os = "linux", target_os = "macos")),
    )?;
    crate::install_cmd::prepare_directory(&options.dir)?;
    if let Some(parent) = resolved.storage_path.parent() {
        crate::install_cmd::prepare_directory(parent)?;
    }
    let trace_seed = resolved
        .compliance
        .as_ref()
        .map(|compliance| read_existing_seed(&options.dir, &compliance.signing_key_file))
        .transpose()?;

    let (identity, created) = crate::loft_key::load_or_create(&options.dir.join("loft.key"))?;
    if created {
        println!(
            "generated a new loft key at {}",
            options.dir.join("loft.key").display()
        );
    }
    let pubkey = identity.verifying_key().to_bytes();
    let identity_seed = Zeroizing::new(identity.to_seed());
    if trace_seed
        .as_ref()
        .is_some_and(|segment_seed| **segment_seed == *identity_seed)
    {
        return Err("trace segment signing key must be separate from the loft identity".into());
    }
    drop(identity_seed);

    let capacity_bytes = resolved
        .capacity_gb
        .checked_mul(1024 * 1024 * 1024)
        .ok_or("capacity is too large")?;
    let mut config = LoftConfig::new(pubkey, &resolved.origin)
        .with_capacity_bytes(capacity_bytes)
        .with_retention_days(resolved.retention_days)
        .with_trusted_proxies(resolved.trusted_proxies.iter().copied());
    if let Some(max_event_bytes) = resolved.max_event_bytes {
        config.max_event_bytes = max_event_bytes;
    }
    if let Some(compliance) = resolved.compliance.as_ref() {
        validate_loft_trace_capacity(compliance, &config)?;
    }
    let trace_records_per_minute = config.global_requests_per_minute;

    let sqlite = Arc::new(SqliteStore::open(
        resolved
            .storage_path
            .to_str()
            .ok_or("non-UTF-8 storage path")?,
    )?);
    let store: Arc<dyn LoftStore> = sqlite.clone();

    let mut loft = Loft::new(config, store)?;
    if let (Some(compliance), Some(segment_seed)) = (resolved.compliance, trace_seed) {
        let cache = WitnessedRegistryKeyCache::new(compliance.registry)
            .map_err(|_| "invalid witnessed compliance registry configuration")?;
        let attribution: Arc<dyn AttributionKeyResolver> = cache.clone();
        let trace_resolver: Arc<dyn TraceKeyResolver> = cache;
        let catalog: Arc<dyn TraceSegmentCatalog> = sqlite;
        let trace_config = SealedTraceConfig {
            directory: compliance.trace_directory,
            jurisdiction: compliance.jurisdiction,
            node_id: pubkey,
            capture_policy: compliance.capture_policy,
            retention_days: compliance.retention_days,
            planned_records_per_minute: trace_records_per_minute,
            capacity_utc_epochs: compliance.capacity_epochs,
            max_records_per_segment: compliance.max_records_per_segment,
            max_storage_bytes: compliance.max_storage_bytes,
        };
        let trace: Arc<dyn TraceSink> = Arc::new(
            SealedTraceSink::new(trace_config, trace_resolver, catalog, *segment_seed)
                .map_err(|_| "loft trace configuration is unavailable")?,
        );
        loft = loft
            .with_attribution_resolver(attribution)
            .with_trace_sink(trace);
    }
    let loft = Arc::new(loft);
    let listener = tokio::net::TcpListener::bind(&resolved.bind).await?;
    println!("loft listening on {}", listener.local_addr()?);
    println!("  pubkey     {}", hex(&pubkey));
    println!(
        "  storage    {} (cap {} GB)",
        resolved.storage_path.display(),
        resolved.capacity_gb
    );
    println!("  retention  {} days", resolved.retention_days);
    pigeonpost_loft::serve(listener, loft, crate::output::shutdown_signal()).await?;

    println!("loft stopped");
    Ok(())
}

fn require_supported_loft_platform(supported: bool) -> Result<(), Box<dyn std::error::Error>> {
    if supported {
        Ok(())
    } else {
        Err("loft service is supported only on Linux, macOS, and Windows".into())
    }
}

fn validate_platform_custody(
    resolved: &ResolvedOptions,
    supports_regulatory_trace: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if resolved.compliance.is_some() && !supports_regulatory_trace {
        return Err(
            "compliance-enabled loft service requires Linux or macOS persistent trace custody; private loft service remains available on Windows"
                .into(),
        );
    }
    Ok(())
}

fn resolve_options(options: &ServeOptions) -> Result<ResolvedOptions, Box<dyn std::error::Error>> {
    let config_path = options.dir.join("loft.toml");
    let installed: Option<InstalledConfig> = load_optional_toml(&config_path)?;

    let bind = options
        .bind
        .clone()
        .or_else(|| installed.as_ref().map(|config| config.loft.bind.clone()))
        .unwrap_or_else(|| DEFAULT_BIND.to_string());
    let bind_address = bind
        .parse::<SocketAddr>()
        .map_err(|_| "bind must be an IP socket address such as 127.0.0.1:7717")?;
    if bind_address.port() == 0 {
        return Err("bind port must not be zero".into());
    }

    let capacity_gb = options
        .capacity_gb
        .or_else(|| installed.as_ref().map(|config| config.loft.capacity_gb))
        .unwrap_or(DEFAULT_CAPACITY_GB);
    if !(1..=MAX_CAPACITY_GB).contains(&capacity_gb) {
        return Err(format!("capacity_gb must be between 1 and {MAX_CAPACITY_GB}").into());
    }

    let retention_days = options
        .retention_days
        .or_else(|| installed.as_ref().map(|config| config.loft.retention_days))
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    if !(1..=MAX_RETENTION_DAYS).contains(&retention_days) {
        return Err(format!("retention_days must be between 1 and {MAX_RETENTION_DAYS}").into());
    }

    let configured_storage_path = installed
        .as_ref()
        .map(|config| config.loft.storage_path.clone())
        .unwrap_or_else(|| PathBuf::from("mail.db"));
    let mut storage_path = resolve_path(&options.dir, &configured_storage_path)?;
    // Versions before 0.2 wrote a directory here. Preserve those installs without silently
    // switching databases.
    if storage_path.is_dir() {
        storage_path = storage_path.join("loft.db");
    }

    let trusted_proxies = installed
        .as_ref()
        .map(|config| config.loft.trusted_proxies.clone())
        .unwrap_or_default();
    if trusted_proxies.len() > MAX_TRUSTED_PROXIES
        || trusted_proxies
            .iter()
            .any(|ip| ip.is_unspecified() || ip.is_multicast())
    {
        return Err(format!(
            "trusted_proxies must contain at most {MAX_TRUSTED_PROXIES} exact usable IP addresses"
        )
        .into());
    }
    let max_event_bytes = installed
        .as_ref()
        .and_then(|config| config.loft.policy.as_ref())
        .map(resolve_installed_policy)
        .transpose()?;
    if let Some(pool) = installed.as_ref().map(|config| &config.pool) {
        validate_installed_pool(pool)?;
    }
    let origin =
        match installed
            .as_ref()
            .and_then(|config| config.pool.domain.as_deref())
            .filter(|domain| !domain.is_empty())
        {
            Some(domain) => format!("https://{domain}"),
            None if bind_address.ip().is_loopback() => format!("http://{bind_address}"),
            None => return Err(
                "a non-loopback loft listener requires pool.domain for origin-bound credentials"
                    .into(),
            ),
        };
    pigeonpost_core::fetch_auth::validate_loft_origin(&origin)
        .map_err(|_| "pool.domain and loft.bind do not form a canonical safe loft origin")?;
    let compliance = installed
        .as_ref()
        .and_then(|config| config.compliance.as_ref())
        .map(|config| resolve_compliance(&options.dir, config))
        .transpose()?;
    let public_exposure = !bind_address.ip().is_loopback()
        || installed.as_ref().is_some_and(|config| {
            config.pool.join
                || config
                    .pool
                    .domain
                    .as_deref()
                    .is_some_and(|domain| !domain.trim().is_empty())
        });
    if public_exposure && compliance.is_none() {
        return Err(
            "a non-loopback loft listener requires complete [compliance.registry] and [compliance.trace] configuration"
                .into(),
        );
    }

    Ok(ResolvedOptions {
        bind,
        origin,
        storage_path,
        capacity_gb,
        retention_days,
        max_event_bytes,
        trusted_proxies,
        compliance,
    })
}

fn resolve_installed_policy(policy: &InstalledPolicy) -> Result<usize, Box<dyn std::error::Error>> {
    if !policy.open || policy.pow_floor != 0 || !policy.allowlist.is_empty() {
        return Err(
            "loft.policy currently supports only open=true, pow_floor=0, and an empty allowlist"
                .into(),
        );
    }
    let ceiling = LoftConfig::new([0u8; 32], "http://127.0.0.1:1").max_event_bytes;
    if policy.max_event_bytes == 0 || policy.max_event_bytes > ceiling {
        return Err(format!("loft.policy.max_event_bytes must be between 1 and {ceiling}").into());
    }
    Ok(policy.max_event_bytes)
}

fn validate_installed_pool(pool: &InstalledPool) -> Result<(), Box<dyn std::error::Error>> {
    let domain = pool.domain.as_deref();
    if domain.is_some_and(|value| {
        value != value.trim()
            || value.len() > 253
            || value.bytes().any(|byte| byte.is_ascii_control())
    }) || (pool.join && domain.is_none_or(|value| value.is_empty()))
    {
        return Err("pool.domain must be a bounded nonempty name when pool.join is true".into());
    }
    if pool
        .directory_url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(
            "pool.directory_url is not consumed by loft serve; use pigeonpost loft submit".into(),
        );
    }
    Ok(())
}

fn resolve_compliance(
    base: &std::path::Path,
    config: &LoftComplianceFile,
) -> Result<ResolvedLoftCompliance, Box<dyn std::error::Error>> {
    validate_segment_limit(config.trace.max_records_per_segment)?;
    let max_storage_bytes = resolve_trace_storage_gib(
        "compliance.trace.max_storage_gb",
        config.trace.max_storage_gb,
    )?;
    let capacity_epochs = config.trace.policy.capacity_epochs()?;
    Ok(ResolvedLoftCompliance {
        registry: config.registry.resolve(base)?,
        jurisdiction: config.trace.policy.jurisdiction,
        capture_policy: config.trace.policy.loft_policy()?,
        retention_days: config.trace.policy.effective_retention_days()?,
        trace_directory: resolve_path(base, &config.trace.directory)?,
        signing_key_file: config.trace.signing_key_file.clone(),
        max_records_per_segment: config.trace.max_records_per_segment,
        max_storage_bytes,
        capacity_epochs,
    })
}

fn validate_loft_trace_capacity(
    compliance: &ResolvedLoftCompliance,
    config: &LoftConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let required_bytes = required_network_trace_storage_bytes(
        config.global_requests_per_minute,
        compliance.capacity_epochs,
        compliance.max_records_per_segment,
    )
    .map_err(|_| {
        "compliance.trace cannot satisfy the configured admission rate and capacity runway"
    })?;
    validate_trace_storage_requirement(
        "compliance.trace.max_storage_gb",
        compliance.max_storage_bytes,
        required_bytes,
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loft_platform_guard_is_fail_closed() {
        assert!(require_supported_loft_platform(false).is_err());
        require_supported_loft_platform(true).unwrap();
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    #[tokio::test]
    async fn unsupported_loft_service_rejects_before_config_or_storage_creation() {
        let root = crate::test_support::private_tempdir();
        let storage = root.path().join("must-not-exist");
        let error = serve(ServeOptions {
            dir: storage.clone(),
            bind: None,
            capacity_gb: None,
            retention_days: None,
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("supported only on Linux, macOS, and Windows"));
        assert!(!storage.exists());
    }

    #[test]
    fn installed_config_is_authoritative_but_flags_override_it() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("loft.toml"),
            r#"[loft]
bind = "127.0.0.1:8817"
storage_path = "data/loft.db"
capacity_gb = 7
retention_days = 41

[loft.policy]
open = true
pow_floor = 0
max_event_bytes = 65536
allowlist = []

[pool]
join = false
"#,
        )
        .unwrap();

        let resolved = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: None,
            capacity_gb: Some(9),
            retention_days: None,
        })
        .unwrap();
        assert_eq!(resolved.bind, "127.0.0.1:8817");
        assert_eq!(resolved.origin, "http://127.0.0.1:8817");
        assert_eq!(resolved.capacity_gb, 9);
        assert_eq!(resolved.retention_days, 41);
        assert_eq!(resolved.max_event_bytes, Some(65_536));
        assert_eq!(resolved.storage_path, dir.path().join("data/loft.db"));
        assert!(resolved.compliance.is_none());
    }

    #[test]
    fn misspelled_public_pool_fields_fail_closed() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("loft.toml"),
            r#"[loft]
bind = "127.0.0.1:7717"
storage_path = "loft.db"
capacity_gb = 20
retention_days = 30

[pool]
joim = true
domain = "loft.example"
"#,
        )
        .unwrap();
        let error = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: None,
            capacity_gb: None,
            retention_days: None,
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("invalid"));
    }

    #[test]
    fn unsafe_values_fail_before_binding_or_opening_storage() {
        let dir = crate::test_support::private_tempdir();
        let error = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: Some("not-an-address".into()),
            capacity_gb: Some(0),
            retention_days: Some(0),
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("bind"));
    }

    #[test]
    fn relative_storage_path_cannot_escape_the_operator_directory() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("loft.toml"),
            r#"[loft]
bind = "127.0.0.1:7717"
storage_path = "../escape.db"
capacity_gb = 20
retention_days = 30

[pool]
join = false
"#,
        )
        .unwrap();
        let error = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: None,
            capacity_gb: None,
            retention_days: None,
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("parent traversal"));
    }

    #[test]
    fn a_public_listener_refuses_to_start_without_complete_compliance_configuration() {
        let dir = crate::test_support::private_tempdir();
        let error = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: Some("0.0.0.0:7717".into()),
            capacity_gb: None,
            retention_days: None,
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("pool.domain"));
        assert!(!dir.path().join("loft.key").exists());
        assert!(!dir.path().join("mail.db").exists());
    }

    #[test]
    fn a_loopback_listener_marked_for_the_public_pool_still_requires_compliance() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("loft.toml"),
            r#"[loft]
bind = "127.0.0.1:7717"
storage_path = "loft.db"
capacity_gb = 20
retention_days = 30

[pool]
join = true
domain = "loft.example"
"#,
        )
        .unwrap();
        let error = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: None,
            capacity_gb: None,
            retention_days: None,
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("requires complete"));
    }

    #[test]
    fn complete_compliance_configuration_resolves_all_relative_paths() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("loft.toml"),
            format!(
                r#"[loft]
bind = "0.0.0.0:7717"
storage_path = "data/loft.db"
capacity_gb = 20
retention_days = 30
trusted_proxies = ["127.0.0.1"]

[pool]
join = true
domain = "loft.example"

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

[compliance.trace]
directory = "compliance/network"
signing_key_file = "secrets/network-signing.key"
max_records_per_segment = 1000
max_storage_gb = 10000

[compliance.trace.policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 730
"#,
                hex(ed25519_dalek::SigningKey::from_bytes(&[1; 32])
                    .verifying_key()
                    .as_bytes(),),
                "22".repeat(32),
                hex(ed25519_dalek::SigningKey::from_bytes(&[2; 32])
                    .verifying_key()
                    .as_bytes(),),
                hex(ed25519_dalek::SigningKey::from_bytes(&[3; 32])
                    .verifying_key()
                    .as_bytes(),),
            ),
        )
        .unwrap();

        let resolved = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: None,
            capacity_gb: None,
            retention_days: None,
        })
        .unwrap();
        let error = validate_platform_custody(&resolved, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("requires Linux or macOS persistent trace custody"));
        assert!(!dir.path().join("loft.key").exists());
        assert!(!dir.path().join("data/loft.db").exists());
        validate_platform_custody(&resolved, true).unwrap();
        let mut compliance = resolved.compliance.unwrap();
        assert_eq!(resolved.origin, "https://loft.example");
        assert_eq!(
            compliance.registry.state_path,
            dir.path().join("compliance/registry-state.json")
        );
        assert_eq!(
            compliance.trace_directory,
            dir.path().join("compliance/network")
        );
        assert_eq!(compliance.max_storage_bytes, 10_000 * 1024 * 1024 * 1024);
        assert_eq!(compliance.capacity_epochs, 731);
        let runtime_config = LoftConfig::new([4; 32], resolved.origin.clone());
        validate_loft_trace_capacity(&compliance, &runtime_config).unwrap();

        let required = required_network_trace_storage_bytes(
            runtime_config.global_requests_per_minute,
            compliance.capacity_epochs,
            compliance.max_records_per_segment,
        )
        .unwrap();
        const GIB_BYTES: u64 = 1024 * 1024 * 1024;
        let minimum_gib = required.div_ceil(GIB_BYTES);
        compliance.max_storage_bytes = (minimum_gib - 1) * GIB_BYTES;
        assert!(validate_loft_trace_capacity(&compliance, &runtime_config).is_err());
        compliance.max_storage_bytes = minimum_gib * GIB_BYTES;
        assert!(validate_loft_trace_capacity(&compliance, &runtime_config).is_ok());
        assert_eq!(
            resolved.trusted_proxies,
            ["127.0.0.1".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn trace_storage_cap_is_mandatory_and_bounded() {
        fn parse(input: &str) -> Result<u64, Box<dyn std::error::Error>> {
            let trace: LoftTraceFile = toml::from_str(input)?;
            resolve_trace_storage_gib("compliance.trace.max_storage_gb", trace.max_storage_gb)
        }

        let fixture = |max_storage: Option<u64>| {
            format!(
                r#"directory = "network"
signing_key_file = "network.key"
max_records_per_segment = 64
{}

[policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 730
"#,
                max_storage
                    .map(|value| format!("max_storage_gb = {value}"))
                    .unwrap_or_default()
            )
        };

        let missing = parse(&fixture(None)).unwrap_err().to_string();
        assert!(missing.contains("max_storage_gb"));

        let too_small = parse(&fixture(Some(0))).unwrap_err().to_string();
        assert!(too_small.contains("compliance.trace.max_storage_gb"));

        let too_large = parse(&fixture(Some(1_048_577))).unwrap_err().to_string();
        assert!(too_large.contains("compliance.trace.max_storage_gb"));
    }

    #[test]
    fn a_partial_compliance_block_is_rejected_during_toml_parsing() {
        let dir = crate::test_support::private_tempdir();
        crate::test_support::write_private(
            dir.path().join("loft.toml"),
            r#"[loft]
bind = "0.0.0.0:7717"
storage_path = "loft.db"
capacity_gb = 20
retention_days = 30

[compliance.registry]
registry_url = "https://registry.example/"
"#,
        )
        .unwrap();
        let error = resolve_options(&ServeOptions {
            dir: dir.path().to_path_buf(),
            bind: None,
            capacity_gb: None,
            retention_days: None,
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("invalid"));
    }
}
