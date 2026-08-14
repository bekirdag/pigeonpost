//! `pigeonpost directory serve` — run the pool directory and its prober.
//!
//! Flat-cost by design: one small box however large the pool gets (`docs/capacity.md`). The prober
//! holds its own agent identity because it measures rather than asks — it writes a test event to
//! each loft and reads it back, which is what catches a node that accepts writes and drops them.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use pigeonpost_core::keys;
use pigeonpost_directory::private_store::PrivateDirectory;
use pigeonpost_directory::{
    serve as serve_directory, Directory, DirectoryHttpConfig, DirectoryLimits, RegistryLogClient,
};
use pigeonpost_registry::{CheckpointPin, RegistryTrust, WitnessKey};
use serde::Deserialize;

use crate::runtime_config::{load_optional_toml, read_existing_seed, WitnessedRegistryFile};

const DIRECTORY_CONFIG: &str = "directory.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryRuntimeFile {
    signing_key_file: Option<PathBuf>,
    #[serde(default)]
    server: DirectoryServerFile,
    registry: WitnessedRegistryFile,
    #[serde(default = "default_witness_wait_seconds")]
    witness_wait_seconds: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryServerFile {
    #[serde(default)]
    trusted_proxies: Vec<IpAddr>,
    #[serde(default)]
    limits: DirectoryLimits,
}

const fn default_witness_wait_seconds() -> u64 {
    15
}

pub async fn serve(bind: &str, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let storage_root = PrivateDirectory::open_or_create(dir)?;
    let dir = storage_root.normalized_path();

    let config_path = dir.join(DIRECTORY_CONFIG);
    let file: DirectoryRuntimeFile = load_optional_toml(&config_path)?.ok_or_else(|| {
        format!(
            "{} is required: directory mutations must use a witnessed registry",
            config_path.display()
        )
    })?;
    let witnessed = file.registry.resolve(dir)?;
    let witnesses = witnessed
        .witnesses
        .iter()
        .map(|witness| {
            let key = keys::verifying_key_from_bytes(&witness.public_key)?;
            Ok(WitnessKey::new(witness.name.clone(), key)?)
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let max_age_secs = witnessed.max_staleness_ms / 1_000;
    let trust = RegistryTrust::new(
        witnessed.expected_origin.clone(),
        witnessed.registry_checkpoint_key,
        witnesses,
        witnessed.witness_threshold,
        CheckpointPin {
            size: witnessed.minimum_checkpoint.size,
            root: witnessed.minimum_checkpoint.root,
        },
        max_age_secs,
        max_age_secs.min(60),
    )?;
    let witness_wait_ms = file
        .witness_wait_seconds
        .checked_mul(1_000)
        .ok_or("witness_wait_seconds is too large")?;
    if witness_wait_ms >= file.server.limits.request_timeout_ms {
        return Err(
            "witness_wait_seconds must be shorter than server.limits.request_timeout_ms".into(),
        );
    }
    let database = dir.join("directory.db");
    let database = database.to_str().ok_or("non-UTF-8 path")?;
    let directory = Arc::new(match file.signing_key_file.as_ref() {
        Some(path) => {
            let seed = read_existing_seed(dir, path)?;
            Directory::open_with_signing_key(database, SigningKey::from_bytes(&seed))?
        }
        None => Directory::open_existing(database)?,
    });
    let registry_log = Arc::new(RegistryLogClient::with_witness_wait(
        &witnessed.registry_url,
        trust,
        Duration::from_secs(file.witness_wait_seconds),
    )?);
    let http = DirectoryHttpConfig::with_trusted_proxies(file.server.trusted_proxies)?
        .with_limits(file.server.limits)?;

    let (identity, created) = crate::loft_key::load_or_create(&dir.join("prober.key"))?;
    let identity = Arc::new(identity);
    if created {
        println!("generated a new prober key");
    }
    directory.verify_registry_logging_ready()?;
    storage_root.verify_named()?;

    let listener = tokio::net::TcpListener::bind(bind).await?;
    let known = directory.entries()?.len();
    println!("directory listening on {bind}");
    println!("  storage     {}", dir.display());
    println!("  prober      {}", identity.address());
    println!("  lofts       {known} known");
    println!("  admission   open — quality is handled by probing, not approval");
    println!(
        "  registry    {} ({}-of-{} witnesses)",
        witnessed.expected_origin,
        witnessed.witness_threshold,
        witnessed.witnesses.len()
    );

    let (stop, stopped) = tokio::sync::watch::channel(false);
    let service = serve_directory(listener, directory, registry_log, http, identity, stopped);
    tokio::pin!(service);
    tokio::select! {
        result = &mut service => {
            result?;
            return Err("directory service stopped unexpectedly".into());
        }
        _ = crate::output::shutdown_signal() => {
            let _ = stop.send(true);
            service.await?;
        }
    }

    storage_root.verify_named()?;
    println!("directory stopped");
    Ok(())
}
