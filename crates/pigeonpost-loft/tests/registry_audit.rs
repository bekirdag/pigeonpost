#![cfg(feature = "server")]

//! Product-path acceptance for the loft's complete witnessed compliance-log audit.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use pigeonpost_compliance_format::{
    attribution_epoch_end_ms, ComplianceKeyId, CompliancePurpose, Jurisdiction,
    TRACE_EPOCH_DURATION_MS,
};
use pigeonpost_loft::{
    AttributionKeyResolver, CheckpointPin, WitnessKeyConfig, WitnessedRegistryConfig,
    WitnessedRegistryKeyCache,
};
use pigeonpost_registry::{
    Checkpoint, ComplianceKeyPublish, ComplianceKeyStatus, LogEntry, MerkleLog,
};
use serde_json::{json, Value};

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

struct Fixture {
    projection: Value,
    entries: Value,
    registry_key: SigningKey,
    witness_key: SigningKey,
    revoked_id: ComplianceKeyId,
    active_id: ComplianceKeyId,
}

fn fixture(now_secs: u64) -> Fixture {
    let registry_key = SigningKey::from_bytes(&[0x71; 32]);
    let witness_key = SigningKey::from_bytes(&[0x72; 32]);
    let now_ms = now_secs.saturating_mul(1_000);
    let day_start_ms = now_ms - now_ms % TRACE_EPOCH_DURATION_MS;
    let (epoch_ms, epoch_end_ms) = (0..=31)
        .find_map(|days_back| {
            let start = day_start_ms.checked_sub(days_back * TRACE_EPOCH_DURATION_MS)?;
            let id = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [0x73; 32],
                start,
                1,
            );
            attribution_epoch_end_ms(&id)
                .ok()
                .filter(|end| now_ms < *end)
                .map(|end| (start, end))
        })
        .expect("current UTC month has a canonical first day");
    let revoked_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0x73; 32],
        epoch_ms,
        1,
    );
    let active_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0x74; 32],
        epoch_ms,
        1,
    );
    let revoked_active = ComplianceKeyPublish {
        key_id: revoked_id,
        public_key: "07".repeat(32),
        not_before_ms: epoch_ms,
        not_after_ms: epoch_end_ms,
        status: ComplianceKeyStatus::Active,
    };
    let still_active = ComplianceKeyPublish {
        key_id: active_id,
        public_key: "08".repeat(32),
        not_before_ms: epoch_ms,
        not_after_ms: epoch_end_ms,
        status: ComplianceKeyStatus::Active,
    };
    let mut revoked = revoked_active.clone();
    revoked.status = ComplianceKeyStatus::Revoked;
    let records = vec![
        LogEntry::compliance_key(0, revoked_active.clone(), epoch_ms),
        LogEntry::compliance_key(1, still_active.clone(), epoch_ms),
        LogEntry::compliance_key(2, revoked, epoch_ms.saturating_add(1)),
    ];
    let mut log = MerkleLog::new();
    for record in &records {
        log.append(&record.leaf_bytes().unwrap());
    }
    let checkpoint = Checkpoint {
        origin: "pigeonpost.test/registry".into(),
        size: records.len() as u64,
        root: log.root(),
    };
    let mut note = checkpoint.sign(&registry_key);
    note.push_str(
        &checkpoint
            .cosignature_line("independent.test", &witness_key, now_secs)
            .unwrap(),
    );
    let projection = json!({
        "tree_size": checkpoint.size,
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        // Both rows have valid inclusion proofs, but this projection hides leaf 2's revocation.
        "keys": [
            {
                "key_id_hex": hex(&revoked_id.encode().unwrap()),
                "publication": revoked_active,
                "log_index": 0,
                "inclusion_path": log.inclusion_proof(0, checkpoint.size).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "entry": records[0],
            },
            {
                "key_id_hex": hex(&active_id.encode().unwrap()),
                "publication": still_active,
                "log_index": 1,
                "inclusion_path": log.inclusion_proof(1, checkpoint.size).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "entry": records[1],
            }
        ],
    });
    let entries = json!({
        "from": 0,
        "to": checkpoint.size,
        "tree_size": checkpoint.size,
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        "entries": records,
    });
    Fixture {
        projection,
        entries,
        registry_key,
        witness_key,
        revoked_id,
        active_id,
    }
}

async fn serve(fixture: &Fixture) -> (String, tokio::task::JoinHandle<()>) {
    let projection = Arc::new(fixture.projection.clone());
    let entries = Arc::new(fixture.entries.clone());
    let app = Router::new()
        .route(
            "/v1/compliance-keys",
            get(move || {
                let projection = Arc::clone(&projection);
                async move { Json((*projection).clone()) }
            }),
        )
        .route(
            "/v1/log/entries",
            get(move || {
                let entries = Arc::clone(&entries);
                async move { Json((*entries).clone()) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}

fn config(
    fixture: &Fixture,
    registry_url: String,
    state_path: std::path::PathBuf,
) -> WitnessedRegistryConfig {
    WitnessedRegistryConfig {
        registry_url,
        expected_origin: "pigeonpost.test/registry".into(),
        registry_checkpoint_key: fixture.registry_key.verifying_key().to_bytes(),
        witnesses: vec![WitnessKeyConfig {
            name: "independent.test".into(),
            public_key: fixture.witness_key.verifying_key().to_bytes(),
        }],
        witness_threshold: 1,
        minimum_checkpoint: CheckpointPin {
            size: 0,
            root: pigeonpost_registry::log::empty_root(),
        },
        max_staleness_ms: 60_000,
        refresh_interval_ms: 10_000,
        state_path,
    }
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    ignore = "persistent witnessed Registry audit cache is supported only on Linux and macOS"
)]
#[tokio::test]
async fn full_log_audit_catches_omitted_revocation_and_survives_restart() {
    let now_secs = now_secs();
    let fixture = fixture(now_secs);
    let (url, server) = serve(&fixture).await;
    let directory = private_tempdir();
    let state_path = directory.path().join("registry-audit.json");
    let cache_config = config(&fixture, url, state_path.clone());
    let cache = WitnessedRegistryKeyCache::new(cache_config.clone()).unwrap();

    cache.refresh_once().await.unwrap();
    assert_eq!(cache.checkpoint().unwrap().size, 3);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&state_path).unwrap().permissions().mode() & 0o077,
            0,
            "the persisted witnessed audit must be owner-only"
        );
    }
    assert!(cache.resolve(&fixture.revoked_id).unwrap().is_none());
    assert_eq!(
        cache
            .resolve(&fixture.active_id)
            .unwrap()
            .unwrap()
            .public_key,
        [8; 32]
    );

    drop(cache);
    server.abort();
    let reopened = WitnessedRegistryKeyCache::new(cache_config.clone()).unwrap();
    assert!(reopened.resolve(&fixture.revoked_id).unwrap().is_none());
    assert_eq!(
        reopened
            .resolve(&fixture.active_id)
            .unwrap()
            .unwrap()
            .public_key,
        [8; 32]
    );

    let mut tampered: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    tampered["audit"]["witnessed_at"] = json!(now_secs.saturating_add(1));
    std::fs::write(&state_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    drop(reopened);
    assert!(WitnessedRegistryKeyCache::new(cache_config).is_err());
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    ignore = "persistent witnessed Registry audit cache is supported only on Linux and macOS"
)]
#[tokio::test]
async fn persistence_failure_does_not_install_the_verified_audit_in_memory() {
    let fixture = fixture(now_secs());
    let (url, server) = serve(&fixture).await;
    let directory = private_tempdir();
    let state_parent = directory.path().join("state");
    std::fs::create_dir(&state_parent).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let state_path = state_parent.join("registry-audit.json");
    let cache = WitnessedRegistryKeyCache::new(config(&fixture, url, state_path)).unwrap();

    // Construction observes an absent destination under a safe directory. Replacing that parent
    // with a regular file then makes the durable write fail after the network audit succeeds.
    std::fs::remove_dir(&state_parent).unwrap();
    std::fs::write(&state_parent, b"not a directory").unwrap();

    assert!(cache.refresh_once().await.is_err());
    assert_eq!(cache.checkpoint(), None);
    assert!(cache.resolve(&fixture.active_id).is_err());
    server.abort();
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    ignore = "persistent witnessed Registry audit cache is supported only on Linux and macOS"
)]
#[test]
fn ipv6_loopback_registry_url_is_accepted() {
    let fixture = fixture(now_secs());
    let directory = private_tempdir();
    let cache = WitnessedRegistryKeyCache::new(config(
        &fixture,
        "http://[::1]:1/".into(),
        directory.path().join("registry-audit.json"),
    ));
    assert!(cache.is_ok());
}

#[cfg(unix)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    ignore = "persistent witnessed Registry audit cache is supported only on Linux and macOS"
)]
#[test]
fn symlinked_registry_audit_is_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = fixture(now_secs());
    let directory = private_tempdir();
    let target = directory.path().join("target.json");
    std::fs::write(&target, b"{}").unwrap();
    let state_path = directory.path().join("registry-audit.json");
    symlink(&target, &state_path).unwrap();

    let cache =
        WitnessedRegistryKeyCache::new(config(&fixture, "http://127.0.0.1:1/".into(), state_path));
    assert!(cache.is_err());
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
