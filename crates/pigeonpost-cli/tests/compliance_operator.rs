//! End-to-end acceptance for the local compliance-key ceremony.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use ed25519_dalek::SigningKey;
use fs2::FileExt;
use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
use pigeonpost_core::{keys, Identity};
use pigeonpost_registry::entry::ComplianceKeyStatus;
use pigeonpost_registry::{Checkpoint, ComplianceKeyQuery, Registry, RegistryConfig};

const ORIGIN: &str = "operator.pigeonpost.test/registry";
const WITNESS_NAME: &str = "independent-test-witness";
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone)]
struct WitnessState {
    operator_key: ed25519_dalek::VerifyingKey,
    witness_key: Arc<SigningKey>,
    monitored: Arc<Mutex<String>>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_publication_resumes_idempotently_then_transitions_under_witnesses() {
    let root = tempfile::tempdir().unwrap();
    let registry_dir = root.path().join("registry");
    std::fs::create_dir(&registry_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&registry_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let checkpoint_seed = [0x31; 32];
    let checkpoint_path = registry_dir.join("checkpoint.key");
    let checkpoint_backup = root.path().join("checkpoint-backup.key");
    write_private(&checkpoint_path, &checkpoint_seed);
    write_private(&checkpoint_backup, &checkpoint_seed);

    let database = registry_dir.join("registry.db");
    Registry::open(
        database.to_str().unwrap(),
        RegistryConfig {
            origin: ORIGIN.into(),
            signing_key: SigningKey::from_bytes(&checkpoint_seed),
            allow_mock_identities: false,
        },
    )
    .unwrap();

    let witness_key = SigningKey::from_bytes(&[0x41; 32]);
    let (unavailable, unavailable_listener) = unavailable_loopback_origin().await;
    write_witness_config(&registry_dir, &unavailable, &witness_key);

    let authority = [0x51; 32];
    let key_id = ComplianceKeyId::new(
        CompliancePurpose::NetworkTrace,
        Jurisdiction::Tr,
        authority,
        0,
        1,
    );
    let key_id_hex = hex(&key_id.encode().unwrap());
    let custody = Identity::from_seed([0x61; 32]);
    let public_key = hex(&keys::x25519_public(&custody));
    let publish_args = publish_args(
        &registry_dir,
        &checkpoint_backup,
        &key_id_hex,
        &hex(&authority),
        &public_key,
    );

    {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let live_service_lock = options.open(registry_dir.join("registry.lock")).unwrap();
        FileExt::lock_exclusive(&live_service_lock).unwrap();
        let blocked = run_pigeonpost(publish_args.clone()).await;
        assert!(!blocked.status.success());
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("already running"));

        let untouched = Registry::open(
            database.to_str().unwrap(),
            RegistryConfig {
                origin: ORIGIN.into(),
                signing_key: SigningKey::from_bytes(&checkpoint_seed),
                allow_mock_identities: false,
            },
        )
        .unwrap();
        assert_eq!(untouched.committed_size().unwrap(), 0);
    }

    let pending = run_pigeonpost(publish_args.clone()).await;
    assert!(!pending.status.success());
    let pending_record = json_record(&pending.stdout);
    assert_eq!(pending_record["result"], "committed_unwitnessed");
    assert_eq!(pending_record["appended"], true);
    assert_eq!(pending_record["committed_size"], 1);
    assert_eq!(pending_record["published_size"], 0);
    drop(unavailable_listener);

    let (witness_url, witness_task) = spawn_witness(checkpoint_seed, witness_key.clone()).await;
    write_witness_config(&registry_dir, &witness_url, &witness_key);
    let resumed = run_pigeonpost(publish_args).await;
    assert!(
        resumed.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed_record = json_record(&resumed.stdout);
    assert_eq!(resumed_record["result"], "witnessed");
    assert_eq!(resumed_record["appended"], false);
    assert_eq!(resumed_record["log_index"], 0);
    assert_eq!(resumed_record["published_size"], 1);

    let retired = run_pigeonpost(transition_args(
        &registry_dir,
        &checkpoint_backup,
        &key_id_hex,
        "retired",
    ))
    .await;
    assert!(
        retired.status.success(),
        "retirement failed: {}",
        String::from_utf8_lossy(&retired.stderr)
    );
    let retired_record = json_record(&retired.stdout);
    assert_eq!(retired_record["result"], "witnessed");
    assert_eq!(retired_record["appended"], true);
    assert_eq!(retired_record["published_size"], 2);

    let retired_retry = run_pigeonpost(transition_args(
        &registry_dir,
        &checkpoint_backup,
        &key_id_hex,
        "retired",
    ))
    .await;
    assert!(retired_retry.status.success());
    let retired_retry_record = json_record(&retired_retry.stdout);
    assert_eq!(retired_retry_record["appended"], false);
    assert_eq!(retired_retry_record["log_index"], 1);
    assert_eq!(retired_retry_record["published_size"], 2);

    let revoked = run_pigeonpost(transition_args(
        &registry_dir,
        &checkpoint_backup,
        &key_id_hex,
        "revoked",
    ))
    .await;
    assert!(
        revoked.status.success(),
        "revocation failed: {}",
        String::from_utf8_lossy(&revoked.stderr)
    );
    let revoked_record = json_record(&revoked.stdout);
    assert_eq!(revoked_record["result"], "witnessed");
    assert_eq!(revoked_record["published_size"], 3);

    let registry = Registry::open(
        database.to_str().unwrap(),
        RegistryConfig {
            origin: ORIGIN.into(),
            signing_key: SigningKey::from_bytes(&checkpoint_seed),
            allow_mock_identities: false,
        },
    )
    .unwrap();
    assert_eq!(registry.committed_size().unwrap(), 3);
    let latest = registry
        .compliance_keys(&ComplianceKeyQuery {
            key_id: Some(key_id),
            include_inactive: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].publication.status, ComplianceKeyStatus::Revoked);

    witness_task.abort();
}

fn publish_args(
    registry_dir: &Path,
    backup: &Path,
    key_id: &str,
    authority: &str,
    public_key: &str,
) -> Vec<String> {
    vec![
        "--json".into(),
        "registry".into(),
        "compliance-key".into(),
        "publish".into(),
        "--dir".into(),
        registry_dir.display().to_string(),
        "--origin".into(),
        ORIGIN.into(),
        "--key-id".into(),
        key_id.into(),
        "--confirm-key-id".into(),
        key_id.into(),
        "--checkpoint-backup".into(),
        backup.display().to_string(),
        "--witness-timeout-seconds".into(),
        "1".into(),
        "--confirm-offline".into(),
        "--execute".into(),
        "--purpose".into(),
        "network-trace".into(),
        "--jurisdiction".into(),
        "tr".into(),
        "--authority".into(),
        authority.into(),
        "--epoch-start-ms".into(),
        "0".into(),
        "--generation".into(),
        "1".into(),
        "--public-key".into(),
        public_key.into(),
        "--not-after-ms".into(),
        DAY_MS.to_string(),
    ]
}

fn transition_args(registry_dir: &Path, backup: &Path, key_id: &str, status: &str) -> Vec<String> {
    vec![
        "--json".into(),
        "registry".into(),
        "compliance-key".into(),
        "transition".into(),
        "--dir".into(),
        registry_dir.display().to_string(),
        "--origin".into(),
        ORIGIN.into(),
        "--key-id".into(),
        key_id.into(),
        "--confirm-key-id".into(),
        key_id.into(),
        "--checkpoint-backup".into(),
        backup.display().to_string(),
        "--witness-timeout-seconds".into(),
        "2".into(),
        "--confirm-offline".into(),
        "--execute".into(),
        "--status".into(),
        status.into(),
    ]
}

async fn run_pigeonpost(args: Vec<String>) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_pigeonpost"))
            .args(args)
            .env_remove("PIGEONPOST_GITHUB_CLIENT_ID")
            .env_remove("PIGEONPOST_GITHUB_CLIENT_SECRET")
            .env_remove("PIGEONPOST_GITHUB_CLIENT_SECRET_FILE")
            .env_remove("PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV")
            .env_remove("PIGEONPOST_GOOGLE_CLIENT_ID")
            .env_remove("PIGEONPOST_ALLOW_MOCK_IDENTITIES")
            .env_remove("PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES")
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn json_record(stdout: &[u8]) -> serde_json::Value {
    serde_json::from_slice(stdout).unwrap_or_else(|error| {
        panic!(
            "operator stdout was not one JSON record ({error}): {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

async fn unavailable_loopback_origin() -> (String, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (format!("http://{address}/"), listener)
}

async fn spawn_witness(
    checkpoint_seed: [u8; 32],
    witness_key: SigningKey,
) -> (String, tokio::task::JoinHandle<()>) {
    let state = WitnessState {
        operator_key: SigningKey::from_bytes(&checkpoint_seed).verifying_key(),
        witness_key: Arc::new(witness_key),
        monitored: Arc::new(Mutex::new(String::new())),
    };
    let app = Router::new()
        .route("/submission/add-checkpoint", post(add_checkpoint))
        .route(
            "/monitoring/{origin_hash}/checkpoint",
            get(monitored_checkpoint),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, task)
}

async fn add_checkpoint(State(state): State<WitnessState>, body: Bytes) -> impl IntoResponse {
    let Ok(request) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, String::new());
    };
    let Some((_proof, operator_note)) = request.split_once("\n\n") else {
        return (StatusCode::BAD_REQUEST, String::new());
    };
    let Ok(checkpoint) = Checkpoint::verify(operator_note, &state.operator_key) else {
        return (StatusCode::FORBIDDEN, String::new());
    };
    if checkpoint.origin != ORIGIN {
        return (StatusCode::UNPROCESSABLE_ENTITY, String::new());
    }
    let Ok(cosignature) = checkpoint.cosignature_line(WITNESS_NAME, &state.witness_key, now_secs())
    else {
        return (StatusCode::INTERNAL_SERVER_ERROR, String::new());
    };
    *state.monitored.lock().unwrap() = format!("{operator_note}{cosignature}");
    (StatusCode::OK, cosignature)
}

async fn monitored_checkpoint(
    State(state): State<WitnessState>,
    AxumPath(_origin_hash): AxumPath<String>,
) -> impl IntoResponse {
    (StatusCode::OK, state.monitored.lock().unwrap().clone())
}

fn write_witness_config(dir: &Path, base: &str, witness: &SigningKey) {
    let config = format!(
        r#"[witnessing]
threshold = 1
max_cosignature_age_seconds = 3600
future_clock_skew_seconds = 30
max_lag_entries = 0
poll_interval_seconds = 1
connect_timeout_seconds = 1
request_timeout_seconds = 1
retry_initial_ms = 10
retry_max_ms = 20
retry_deadline_seconds = 1

[[witnessing.witnesses]]
name = "{WITNESS_NAME}"
public_key = "{}"
submission_prefix = "{base}submission/"
monitoring_prefix = "{base}monitoring/"
"#,
        hex(witness.verifying_key().as_bytes())
    );
    std::fs::write(dir.join("registry.toml"), config).unwrap();
}

fn write_private(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
