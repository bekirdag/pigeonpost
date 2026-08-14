//! Whole-process acceptance gate for the SDS log-privacy invariant.
//!
//! The test intentionally launches the production binary instead of in-process routers. This
//! captures startup/runtime stdout and stderr, while raw TCP clients let the test know the exact
//! source address presented to each service. A witnessed registry fixture activates the real
#![cfg(all(
    feature = "test-utilities",
    any(target_os = "linux", target_os = "macos")
))]
//! sealed-trace paths; a pass therefore cannot be obtained by silently disabling trace capture.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use ed25519_dalek::{Signer, SigningKey};
use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
use pigeonpost_compliance_seal::TRACE_EPOCH_DURATION_MS;
use pigeonpost_core::envelope::wrap;
use pigeonpost_core::{keys, Identity};
use pigeonpost_registry::entry::claim_payload;
use pigeonpost_registry::{
    Checkpoint, ComplianceKeyPublish, ComplianceKeyQuery, ComplianceKeyStatus, LogEntry, MerkleLog,
    Registry, RegistryConfig,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::{TcpSocket, TcpStream};

// macOS does not route the entire 127/8 block without explicit aliases. Both local roles use
// 127.0.0.1 so the acceptance test remains portable; the exact ephemeral socket distinguishes
// direct connections, while Forwarded carries the independently recognisable edge client.
const DIRECT_CLIENT_IP: &str = "127.0.0.1";
const TRUSTED_EDGE_IP: &str = "127.0.0.1";
const FORWARDED_CLIENT_IP: &str = "198.51.100.77";
const FORWARDED_CLIENT_PORT: u16 = 42_424;
const DISCLOSURE_SELECTOR: &str = "H21_RAW_DISCLOSURE_SELECTOR_9F7E";
const PANIC_CLIENT_IP: &str = "203.0.113.99";
const PANIC_CLIENT_PORT: u16 = 43_434;
const WITNESS_NAME: &str = "h21-independent";
const REGISTRY_ORIGIN: &str = "h21.pigeonpost.test/registry";

#[derive(Clone)]
struct WitnessedFixture {
    entries: Arc<Vec<LogEntry>>,
    log: Arc<MerkleLog>,
    checkpoint: Arc<String>,
    root_hex: Arc<String>,
    keys: Arc<Vec<FixtureKey>>,
    operator_key: ed25519_dalek::VerifyingKey,
    witness: Arc<SigningKey>,
}

#[derive(Clone)]
struct FixtureKey {
    key_id_hex: String,
    publication: ComplianceKeyPublish,
    log_index: u64,
    inclusion_path: Vec<String>,
    entry: LogEntry,
}

#[derive(Debug, Default, Deserialize)]
struct FixtureComplianceQuery {
    #[serde(default)]
    include_entries: bool,
    #[serde(default)]
    metadata_only: bool,
}

#[derive(Debug, Deserialize)]
struct FixtureEntriesQuery {
    #[serde(default)]
    from: u64,
    to: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FixtureConsistencyQuery {
    from: u64,
}

struct FixtureServer {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl FixtureServer {
    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.await.expect("witnessed fixture task panicked");
    }
}

struct ServiceProcess {
    name: &'static str,
    child: Option<Child>,
}

struct LoopbackReservation {
    listener: TcpListener,
    address: SocketAddr,
}

impl LoopbackReservation {
    fn new() -> Self {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve loopback service address");
        let address = listener.local_addr().expect("reserved loopback address");
        Self { listener, address }
    }

    fn address(&self) -> SocketAddr {
        self.address
    }

    fn release(self) {
        drop(self.listener);
    }
}

impl ServiceProcess {
    fn spawn(name: &'static str, args: &[String], env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pigeonpost"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PIGEONPOST_LOG", "trace")
            .env("RUST_BACKTRACE", "full")
            .env_remove("PIGEONPOST_GITHUB_CLIENT_ID")
            .env_remove("PIGEONPOST_GITHUB_CLIENT_SECRET")
            .env_remove("PIGEONPOST_GITHUB_CLIENT_SECRET_FILE")
            .env_remove("PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV")
            .env_remove("PIGEONPOST_GOOGLE_CLIENT_ID")
            .env_remove("PIGEONPOST_ALLOW_MOCK_IDENTITIES")
            .env_remove("PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES");
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .unwrap_or_else(|error| panic!("failed to launch {name} production process: {error}"));
        Self {
            name,
            child: Some(child),
        }
    }

    fn stop(mut self) -> Output {
        let mut child = self.child.take().expect("service process already consumed");
        interrupt(&mut child);
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    child
                        .kill()
                        .unwrap_or_else(|error| panic!("failed to kill {}: {error}", self.name));
                    break;
                }
                Err(error) => panic!("failed to inspect {}: {error}", self.name),
            }
        }
        child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("failed to collect {} output: {error}", self.name))
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct RawResponse {
    text: String,
    local_addr: SocketAddr,
}

#[derive(Clone)]
struct ForbiddenValue {
    label: String,
    value: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whole_process_output_never_discloses_client_network_data_or_selectors() {
    let root = tempfile::tempdir().expect("temporary H-21 root");
    let operator_seed = [0x41; 32];
    let witness = SigningKey::from_bytes(&[0x42; 32]);
    let registry_dir = root.path().join("registry");
    let fixture_state = build_registry_fixture(&registry_dir, operator_seed, &witness);
    let minimum_checkpoint_size = fixture_state.entries.len() as u64;
    let minimum_checkpoint_root = fixture_state.root_hex.to_string();
    let fixture = spawn_witnessed_fixture(fixture_state).await;

    // Keep every socket reserved at once so the kernel cannot recycle one service's port for a
    // sibling before the production processes bind. Each reservation is released only when its
    // corresponding process is ready to start.
    let registry_reservation = LoopbackReservation::new();
    let loft_reservation = LoopbackReservation::new();
    let directory_reservation = LoopbackReservation::new();
    let registry_addr = registry_reservation.address();
    let loft_addr = loft_reservation.address();
    let directory_addr = directory_reservation.address();
    let directory_dir = root.path().join("directory");
    fs::create_dir_all(&directory_dir).expect("directory runtime root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory_dir, fs::Permissions::from_mode(0o700))
            .expect("private directory runtime root");
    }
    let directory_seed = [0x46; 32];
    write_seed(
        &directory_dir.join("directory-signing.key"),
        &directory_seed,
    );
    let directory_key = SigningKey::from_bytes(&directory_seed)
        .verifying_key()
        .to_bytes();
    write_registry_config(
        &registry_dir,
        &fixture.url,
        operator_seed,
        &witness,
        directory_key,
        (minimum_checkpoint_size, &minimum_checkpoint_root),
        false,
    );

    let loft_dir = root.path().join("loft");
    write_loft_config(
        &loft_dir,
        loft_addr,
        &fixture.url,
        operator_seed,
        &witness,
        (minimum_checkpoint_size, &minimum_checkpoint_root),
        false,
    );
    write_directory_config(
        &directory_dir,
        &fixture.url,
        operator_seed,
        &witness,
        minimum_checkpoint_size,
        &minimum_checkpoint_root,
    );

    let registry_args = vec![
        "registry".into(),
        "serve".into(),
        "--bind".into(),
        registry_addr.to_string(),
        "--dir".into(),
        registry_dir.display().to_string(),
        "--origin".into(),
        REGISTRY_ORIGIN.into(),
    ];
    let loft_args = vec![
        "loft".into(),
        "serve".into(),
        "--dir".into(),
        loft_dir.display().to_string(),
    ];
    let directory_args = vec![
        "directory".into(),
        "serve".into(),
        "--bind".into(),
        directory_addr.to_string(),
        "--dir".into(),
        directory_dir.display().to_string(),
    ];

    registry_reservation.release();
    let mut registry = ServiceProcess::spawn(
        "registry",
        &registry_args,
        &[("PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES", "1")],
    );
    wait_until_ready(&mut registry, registry_addr, "/health").await;

    loft_reservation.release();
    let mut loft = ServiceProcess::spawn("loft", &loft_args, &[]);
    wait_until_ready(&mut loft, loft_addr, "/ready").await;

    directory_reservation.release();
    let mut directory = ServiceProcess::spawn("directory", &directory_args, &[]);
    wait_until_ready(&mut directory, directory_addr, "/health").await;

    let mut surfaces: Vec<(String, String)> = Vec::new();
    let mut forbidden = base_forbidden_values();

    drive_registry(
        registry_addr,
        0,
        false,
        false,
        &mut surfaces,
        &mut forbidden,
    )
    .await;
    drive_loft(loft_addr, 0, false, false, &mut surfaces, &mut forbidden).await;
    drive_directory(directory_addr, &mut surfaces, &mut forbidden).await;

    let registry_direct_output = registry.stop();
    let loft_direct_output = loft.stop();
    assert_clean_exit("direct registry", &registry_direct_output);
    assert_clean_exit("direct loft", &loft_direct_output);
    surfaces.extend(process_surfaces("direct registry", &registry_direct_output));
    surfaces.extend(process_surfaces("direct loft", &loft_direct_output));

    let registry_proxy_reservation = LoopbackReservation::new();
    let loft_proxy_reservation = LoopbackReservation::new();
    let registry_proxy_addr = registry_proxy_reservation.address();
    let loft_proxy_addr = loft_proxy_reservation.address();
    write_registry_config(
        &registry_dir,
        &fixture.url,
        operator_seed,
        &witness,
        directory_key,
        (minimum_checkpoint_size, &minimum_checkpoint_root),
        true,
    );
    write_loft_config(
        &loft_dir,
        loft_proxy_addr,
        &fixture.url,
        operator_seed,
        &witness,
        (minimum_checkpoint_size, &minimum_checkpoint_root),
        true,
    );
    let registry_proxy_args = vec![
        "registry".into(),
        "serve".into(),
        "--bind".into(),
        registry_proxy_addr.to_string(),
        "--dir".into(),
        registry_dir.display().to_string(),
        "--origin".into(),
        REGISTRY_ORIGIN.into(),
    ];
    registry_proxy_reservation.release();
    let mut registry = ServiceProcess::spawn(
        "trusted-proxy registry",
        &registry_proxy_args,
        &[("PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES", "1")],
    );
    wait_until_ready(&mut registry, registry_proxy_addr, "/health").await;

    loft_proxy_reservation.release();
    let mut loft = ServiceProcess::spawn("trusted-proxy loft", &loft_args, &[]);
    wait_until_ready(&mut loft, loft_proxy_addr, "/ready").await;
    drive_registry(
        registry_proxy_addr,
        1,
        true,
        true,
        &mut surfaces,
        &mut forbidden,
    )
    .await;
    drive_loft(
        loft_proxy_addr,
        1,
        true,
        true,
        &mut surfaces,
        &mut forbidden,
    )
    .await;

    let registry_proxy_output = registry.stop();
    let loft_proxy_output = loft.stop();
    let directory_output = directory.stop();
    assert_clean_exit("trusted-proxy registry", &registry_proxy_output);
    assert_clean_exit("trusted-proxy loft", &loft_proxy_output);
    assert_clean_exit("directory", &directory_output);
    surfaces.extend(process_surfaces(
        "trusted-proxy registry",
        &registry_proxy_output,
    ));
    surfaces.extend(process_surfaces("trusted-proxy loft", &loft_proxy_output));
    surfaces.extend(process_surfaces("directory", &directory_output));

    assert_closed_trace(&loft_dir.join("traces"), "loft network trace");
    assert_closed_trace(
        &registry_dir.join("network-traces"),
        "registry network claim trace",
    );
    assert_closed_trace(
        &registry_dir.join("identity-traces"),
        "registry identity claim trace",
    );

    let panic_output = run_error_and_panic_child();
    assert!(
        !panic_output.status.success(),
        "H-21 crash probe must terminate unsuccessfully"
    );
    let panic_stderr = String::from_utf8_lossy(&panic_output.stderr).into_owned();
    assert!(panic_stderr.contains("loft internal error"));
    assert!(panic_stderr.contains("registry internal error"));
    assert!(panic_stderr.contains("directory internal error"));
    assert!(panic_stderr.contains("forced H-21 generic panic probe"));
    surfaces.extend(process_surfaces("error-and-panic child", &panic_output));

    if std::env::var_os("PIGEONPOST_H21_INJECT_TEST_LEAK").is_some() {
        surfaces.push((
            "test-only deliberate leak injection".into(),
            format!("edge_client={FORWARDED_CLIENT_IP}:{FORWARDED_CLIENT_PORT}"),
        ));
    }
    assert_private_surfaces(&surfaces, &forbidden);
    fixture.stop().await;
}

#[test]
fn privacy_scanner_rejects_every_protected_value_class() {
    let forbidden = base_forbidden_values();
    for item in &forbidden {
        let surface = vec![(
            "synthetic leak".into(),
            format!("before {} after", item.value),
        )];
        let violations = privacy_violations(&surface, &forbidden);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains(&item.label)),
            "scanner did not reject {}",
            item.label
        );
    }
}

#[test]
fn simultaneous_loopback_reservations_are_distinct_and_exclusive() {
    let first = LoopbackReservation::new();
    let second = LoopbackReservation::new();
    let third = LoopbackReservation::new();
    let addresses = [first.address(), second.address(), third.address()];

    assert_ne!(addresses[0], addresses[1]);
    assert_ne!(addresses[0], addresses[2]);
    assert_ne!(addresses[1], addresses[2]);
    for address in addresses {
        let error = TcpListener::bind(address).expect_err("held reservation must remain exclusive");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    }
}

#[test]
#[ignore = "spawned by the whole-process H-21 parent test"]
fn h21_internal_error_and_panic_child() {
    if std::env::var("PIGEONPOST_H21_PANIC_CHILD").as_deref() != Ok("1") {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(io::stderr)
        .try_init();

    let secret = format!("{DISCLOSURE_SELECTOR} source={PANIC_CLIENT_IP}:{PANIC_CLIENT_PORT}");
    let _ = pigeonpost_loft::LoftError::Io(io::Error::other(secret.clone())).into_response();
    let _ =
        pigeonpost_registry::RegistryError::InvalidConfiguration(secret.clone()).into_response();
    let _ =
        pigeonpost_directory::DirectoryError::Io(io::Error::other(secret.clone())).into_response();
    std::hint::black_box(&secret);
    panic!("forced H-21 generic panic probe");
}

fn build_registry_fixture(
    registry_dir: &Path,
    operator_seed: [u8; 32],
    witness: &SigningKey,
) -> WitnessedFixture {
    fs::create_dir_all(registry_dir).expect("registry runtime root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(registry_dir, fs::Permissions::from_mode(0o700))
            .expect("set private registry runtime permissions");
    }
    write_seed(&registry_dir.join("checkpoint.key"), &operator_seed);
    write_seed(&registry_dir.join("network-signing.key"), &[0x43; 32]);
    write_seed(&registry_dir.join("identity-signing.key"), &[0x44; 32]);

    let database = registry_dir.join("registry.db");
    let registry = Registry::open(
        database.to_str().expect("UTF-8 fixture database path"),
        RegistryConfig {
            origin: REGISTRY_ORIGIN.into(),
            signing_key: SigningKey::from_bytes(&operator_seed),
            allow_mock_identities: true,
        },
    )
    .expect("open compliance-key fixture registry");
    let current_ms = now_ms();
    let current_epoch = current_ms - current_ms % TRACE_EPOCH_DURATION_MS;
    for epoch in [current_epoch, current_epoch + TRACE_EPOCH_DURATION_MS] {
        publish_trace_key(
            &registry,
            CompliancePurpose::NetworkTrace,
            epoch,
            [0x51; 32],
            [0x61; 32],
        );
        publish_trace_key(
            &registry,
            CompliancePurpose::IdentityTrace,
            epoch,
            [0x52; 32],
            [0x62; 32],
        );
    }

    let set = registry
        .compliance_key_set(&ComplianceKeyQuery {
            include_inactive: true,
            ..ComplianceKeyQuery::default()
        })
        .expect("read compliance-key projection");
    let entries = registry
        .entries(0, set.head.size)
        .expect("read compliance fixture log");
    let mut log = MerkleLog::new();
    for entry in &entries {
        log.append(&entry.leaf_bytes().expect("canonical fixture leaf"));
    }
    assert_eq!(log.root(), set.head.root);
    let checkpoint = Checkpoint {
        origin: REGISTRY_ORIGIN.into(),
        size: set.head.size,
        root: set.head.root,
    };
    let operator = SigningKey::from_bytes(&operator_seed);
    let mut witnessed_note = checkpoint.sign(&operator);
    witnessed_note.push_str(
        &checkpoint
            .cosignature_line(WITNESS_NAME, witness, now_secs())
            .expect("witness fixture checkpoint"),
    );
    let keys = set
        .keys
        .into_iter()
        .map(|key| FixtureKey {
            key_id_hex: hex(&key
                .publication
                .key_id
                .encode()
                .expect("canonical compliance key id")),
            publication: key.publication,
            log_index: key.index,
            inclusion_path: key.inclusion.path.iter().map(|hash| hex(hash)).collect(),
            entry: key.entry,
        })
        .collect();
    drop(registry);
    WitnessedFixture {
        entries: Arc::new(entries),
        log: Arc::new(log),
        checkpoint: Arc::new(witnessed_note),
        root_hex: Arc::new(hex(&checkpoint.root)),
        keys: Arc::new(keys),
        operator_key: operator.verifying_key(),
        witness: Arc::new(witness.clone()),
    }
}

fn publish_trace_key(
    registry: &Registry,
    purpose: CompliancePurpose,
    epoch_start_ms: u64,
    authority: [u8; 32],
    custody_seed: [u8; 32],
) {
    let key_id = ComplianceKeyId::new(purpose, Jurisdiction::Tr, authority, epoch_start_ms, 1);
    let custody = Identity::from_seed(custody_seed);
    registry
        .publish_compliance_key(ComplianceKeyPublish {
            key_id,
            public_key: hex(&keys::x25519_public(&custody)),
            not_before_ms: epoch_start_ms,
            not_after_ms: epoch_start_ms + TRACE_EPOCH_DURATION_MS,
            status: ComplianceKeyStatus::Active,
        })
        .expect("publish fixture trace key");
}

async fn spawn_witnessed_fixture(state: WitnessedFixture) -> FixtureServer {
    let app = Router::new()
        .route("/v1/compliance-keys", get(fixture_compliance_keys))
        .route("/v1/log/entries", get(fixture_entries))
        .route("/v1/log/consistency", get(fixture_consistency))
        .route("/submission/add-checkpoint", post(fixture_add_checkpoint))
        .route(
            "/monitoring/{origin_hash}/checkpoint",
            get(fixture_monitored_checkpoint),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind witnessed fixture");
    let url = format!(
        "http://{}/",
        listener.local_addr().expect("fixture address")
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
            .expect("serve witnessed fixture");
    });
    FixtureServer {
        url,
        stop: Some(stop_tx),
        task,
    }
}

async fn fixture_add_checkpoint(
    State(state): State<WitnessedFixture>,
    body: Bytes,
) -> impl IntoResponse {
    let Ok(text) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, String::new());
    };
    let Some((_proof, operator_note)) = text.split_once("\n\n") else {
        return (StatusCode::BAD_REQUEST, String::new());
    };
    let Ok(checkpoint) = Checkpoint::verify(operator_note, &state.operator_key) else {
        return (StatusCode::FORBIDDEN, String::new());
    };
    if checkpoint.origin != REGISTRY_ORIGIN {
        return (StatusCode::UNPROCESSABLE_ENTITY, String::new());
    }
    let Ok(cosignature) = checkpoint.cosignature_line(WITNESS_NAME, &state.witness, now_secs())
    else {
        return (StatusCode::INTERNAL_SERVER_ERROR, String::new());
    };
    (StatusCode::OK, cosignature)
}

async fn fixture_monitored_checkpoint(
    State(state): State<WitnessedFixture>,
    AxumPath(_origin_hash): AxumPath<String>,
) -> impl IntoResponse {
    (StatusCode::OK, state.checkpoint.as_str().to_owned())
}

async fn fixture_compliance_keys(
    State(state): State<WitnessedFixture>,
    Query(query): Query<FixtureComplianceQuery>,
) -> Json<Value> {
    let keys = if query.metadata_only {
        Vec::new()
    } else {
        state
            .keys
            .iter()
            .map(|key| {
                let mut object = serde_json::Map::new();
                object.insert("key_id_hex".into(), json!(key.key_id_hex));
                object.insert("publication".into(), json!(key.publication));
                object.insert("log_index".into(), json!(key.log_index));
                object.insert("inclusion_path".into(), json!(key.inclusion_path));
                if query.include_entries {
                    object.insert("entry".into(), json!(key.entry));
                }
                Value::Object(object)
            })
            .collect()
    };
    Json(json!({
        "tree_size": state.entries.len(),
        "root": state.root_hex.as_str(),
        "checkpoint": state.checkpoint.as_str(),
        "keys": keys,
    }))
}

async fn fixture_entries(
    State(state): State<WitnessedFixture>,
    Query(query): Query<FixtureEntriesQuery>,
) -> Json<Value> {
    let size = state.entries.len() as u64;
    let to = query.to.unwrap_or(size).min(size);
    let from = query.from.min(to);
    let entries = state.entries[from as usize..to as usize].to_vec();
    Json(json!({
        "from": from,
        "to": to,
        "tree_size": size,
        "root": state.root_hex.as_str(),
        "checkpoint": state.checkpoint.as_str(),
        "entries": entries,
    }))
}

async fn fixture_consistency(
    State(state): State<WitnessedFixture>,
    Query(query): Query<FixtureConsistencyQuery>,
) -> Json<Value> {
    let size = state.entries.len() as u64;
    let path = state
        .log
        .consistency_proof(query.from, size)
        .unwrap_or_default()
        .iter()
        .map(|hash| hex(hash))
        .collect::<Vec<_>>();
    Json(json!({
        "from": query.from,
        "to": size,
        "root": state.root_hex.as_str(),
        "path": path,
    }))
}

fn write_registry_config(
    dir: &Path,
    fixture_url: &str,
    operator_seed: [u8; 32],
    witness: &SigningKey,
    directory_publisher_key: [u8; 32],
    minimum_checkpoint: (u64, &str),
    trust_loopback_proxy: bool,
) {
    let (minimum_checkpoint_size, minimum_checkpoint_root) = minimum_checkpoint;
    let operator_key = SigningKey::from_bytes(&operator_seed).verifying_key();
    let trusted_proxies = if trust_loopback_proxy {
        format!("[\"{TRUSTED_EDGE_IP}\"]")
    } else {
        "[]".into()
    };
    let config = format!(
        r#"[server]
trusted_proxies = {trusted_proxies}
directory_publisher_keys = ["{}"]

[compliance.registry]
registry_url = "{fixture_url}"
expected_origin = "{REGISTRY_ORIGIN}"
registry_checkpoint_key = "{}"
witness_threshold = 1
minimum_checkpoint_size = {minimum_checkpoint_size}
minimum_checkpoint_root = "{minimum_checkpoint_root}"
max_staleness_seconds = 3600
refresh_interval_seconds = 60
state_path = "witnessed-keys.json"

[[compliance.registry.witnesses]]
name = "{WITNESS_NAME}"
public_key = "{}"

[compliance.claim_trace]
network_directory = "network-traces"
identity_directory = "identity-traces"
network_signing_key_file = "network-signing.key"
identity_signing_key_file = "identity-signing.key"
max_records_per_segment = 1000
network_max_storage_gb = 10000
identity_max_storage_gb = 10000

[compliance.claim_trace.policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 730

[witnessing]
threshold = 1
max_cosignature_age_seconds = 3600
future_clock_skew_seconds = 30
max_lag_entries = 0
poll_interval_seconds = 60
connect_timeout_seconds = 1
request_timeout_seconds = 2
retry_initial_ms = 10
retry_max_ms = 50
retry_deadline_seconds = 2

[[witnessing.witnesses]]
name = "{WITNESS_NAME}"
public_key = "{}"
submission_prefix = "{fixture_url}submission/"
monitoring_prefix = "{fixture_url}monitoring/"
"#,
        hex(&directory_publisher_key),
        hex(operator_key.as_bytes()),
        hex(witness.verifying_key().as_bytes()),
        hex(witness.verifying_key().as_bytes()),
    );
    fs::write(dir.join("registry.toml"), config).expect("write registry config");
}

fn write_loft_config(
    dir: &Path,
    bind: SocketAddr,
    fixture_url: &str,
    operator_seed: [u8; 32],
    witness: &SigningKey,
    minimum_checkpoint: (u64, &str),
    trust_loopback_proxy: bool,
) {
    let (minimum_checkpoint_size, minimum_checkpoint_root) = minimum_checkpoint;
    fs::create_dir_all(dir).expect("loft runtime root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .expect("private loft runtime root");
    }
    write_seed(&dir.join("trace-signing.key"), &[0x45; 32]);
    let operator_key = SigningKey::from_bytes(&operator_seed).verifying_key();
    let trusted_proxies = if trust_loopback_proxy {
        format!("[\"{TRUSTED_EDGE_IP}\"]")
    } else {
        "[]".into()
    };
    let config = format!(
        r#"[loft]
bind = "{bind}"
storage_path = "loft.db"
capacity_gb = 1
retention_days = 1
trusted_proxies = {trusted_proxies}

[loft.policy]
open = true
pow_floor = 0
max_event_bytes = 65536
allowlist = []

[pool]
join = false

[compliance.registry]
registry_url = "{fixture_url}"
expected_origin = "{REGISTRY_ORIGIN}"
registry_checkpoint_key = "{}"
witness_threshold = 1
minimum_checkpoint_size = {minimum_checkpoint_size}
minimum_checkpoint_root = "{minimum_checkpoint_root}"
max_staleness_seconds = 3600
refresh_interval_seconds = 60
state_path = "witnessed-keys.json"

[[compliance.registry.witnesses]]
name = "{WITNESS_NAME}"
public_key = "{}"

[compliance.trace]
directory = "traces"
signing_key_file = "trace-signing.key"
max_records_per_segment = 1000
max_storage_gb = 10000

[compliance.trace.policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 730
"#,
        hex(operator_key.as_bytes()),
        hex(witness.verifying_key().as_bytes()),
    );
    fs::write(dir.join("loft.toml"), config).expect("write loft config");
}

fn write_directory_config(
    dir: &Path,
    fixture_url: &str,
    operator_seed: [u8; 32],
    witness: &SigningKey,
    minimum_checkpoint_size: u64,
    minimum_checkpoint_root: &str,
) {
    let operator_key = SigningKey::from_bytes(&operator_seed).verifying_key();
    let config = format!(
        r#"witness_wait_seconds = 1
signing_key_file = "directory-signing.key"

[registry]
registry_url = "{fixture_url}"
expected_origin = "{REGISTRY_ORIGIN}"
registry_checkpoint_key = "{}"
witness_threshold = 1
minimum_checkpoint_size = {minimum_checkpoint_size}
minimum_checkpoint_root = "{minimum_checkpoint_root}"
max_staleness_seconds = 3600
refresh_interval_seconds = 60
state_path = "registry-state/witnessed-keys.json"

[[registry.witnesses]]
name = "{WITNESS_NAME}"
public_key = "{}"
"#,
        hex(operator_key.as_bytes()),
        hex(witness.verifying_key().as_bytes()),
    );
    fs::write(dir.join("directory.toml"), config).expect("write directory config");
}

async fn drive_registry(
    address: SocketAddr,
    index: usize,
    forwarded: bool,
    include_observability_surfaces: bool,
    surfaces: &mut Vec<(String, String)>,
    forbidden: &mut Vec<ForbiddenValue>,
) {
    let claimant = SigningKey::from_bytes(&[0x70 + index as u8; 32]);
    let name = format!("h21claim{index}");
    let handle = format!("/github/{name}");
    let pubkey = claimant.verifying_key().to_bytes();
    let signature = claimant.sign(&claim_payload(&handle, &pubkey));
    let body = serde_json::to_vec(&json!({
        "handle": handle,
        "pubkey": hex(&pubkey),
        "signature": hex(&signature.to_bytes()),
        "proof": {"provider": "mock", "name": name},
    }))
    .expect("registry request JSON");
    let forwarded_value = format!("for=\"{FORWARDED_CLIENT_IP}:{FORWARDED_CLIENT_PORT}\"");
    let headers = if forwarded {
        vec![("Forwarded", forwarded_value.as_str())]
    } else {
        Vec::new()
    };
    let response = raw_http(
        address,
        DIRECT_CLIENT_IP.parse().expect("fixture source IP"),
        "POST",
        "/v1/register",
        &headers,
        &body,
    )
    .await;
    assert_status("registry registration", &response.text, StatusCode::OK);
    remember_source(forbidden, "registry", response.local_addr);
    surfaces.push((
        format!("registry registration response {index}"),
        response.text,
    ));

    if !include_observability_surfaces {
        return;
    }

    let malformed =
        format!("{{\"handle\":\"/{DISCLOSURE_SELECTOR}\",\"pubkey\":\"{DISCLOSURE_SELECTOR}\"}}");
    let response = raw_http(
        address,
        TRUSTED_EDGE_IP.parse().unwrap(),
        "POST",
        "/v1/register",
        &[
            (
                "Forwarded",
                &format!("for=\"{FORWARDED_CLIENT_IP}:{FORWARDED_CLIENT_PORT}\""),
            ),
            ("X-H21-Disclosure-Selector", DISCLOSURE_SELECTOR),
        ],
        malformed.as_bytes(),
    )
    .await;
    assert!(response.text.starts_with("HTTP/1.1 4"));
    remember_source(forbidden, "registry malformed", response.local_addr);

    for (label, path) in [
        ("registry checkpoint", "/v1/log/checkpoint"),
        ("registry public leaves", "/v1/log/dump"),
        ("registry metrics", "/metrics"),
    ] {
        let response = raw_http(
            address,
            DIRECT_CLIENT_IP.parse().unwrap(),
            "GET",
            path,
            &[("X-H21-Disclosure-Selector", DISCLOSURE_SELECTOR)],
            &[],
        )
        .await;
        remember_source(forbidden, label, response.local_addr);
        surfaces.push((label.into(), response.text));
    }
}

async fn drive_loft(
    address: SocketAddr,
    index: usize,
    forwarded: bool,
    include_observability_surfaces: bool,
    surfaces: &mut Vec<(String, String)>,
    forbidden: &mut Vec<ForbiddenValue>,
) {
    let sender = Identity::from_seed([0x73; 32]);
    let recipient = Identity::from_seed([0x74; 32]);
    let recipient_selector = hex(recipient.verifying_key().as_bytes());
    forbidden.push(ForbiddenValue {
        label: "raw loft recipient selector".into(),
        value: recipient_selector,
    });

    let wrapped = wrap(
        &sender,
        &recipient.verifying_key(),
        &format!("H-21 ordinary message {index}"),
        now_secs(),
    )
    .expect("build valid wrap");
    let body = serde_json::to_vec(&json!({"wrap": wrapped})).expect("loft publish request JSON");
    let forwarded_value = format!("for=\"{FORWARDED_CLIENT_IP}:{FORWARDED_CLIENT_PORT}\"");
    let headers = if forwarded {
        vec![("Forwarded", forwarded_value.as_str())]
    } else {
        Vec::new()
    };
    let response = raw_http(
        address,
        DIRECT_CLIENT_IP.parse().unwrap(),
        "POST",
        "/v1/publish",
        &headers,
        &body,
    )
    .await;
    assert_status("loft publish", &response.text, StatusCode::OK);
    remember_source(forbidden, "loft", response.local_addr);
    surfaces.push((format!("loft publish response {index}"), response.text));

    if !include_observability_surfaces {
        return;
    }

    let malformed = format!("{{\"wrap\":\"{DISCLOSURE_SELECTOR}\"}}");
    let response = raw_http(
        address,
        TRUSTED_EDGE_IP.parse().unwrap(),
        "POST",
        "/v1/publish",
        &[
            (
                "Forwarded",
                &format!("for=\"{FORWARDED_CLIENT_IP}:{FORWARDED_CLIENT_PORT}\""),
            ),
            ("X-H21-Disclosure-Selector", DISCLOSURE_SELECTOR),
        ],
        malformed.as_bytes(),
    )
    .await;
    assert!(response.text.starts_with("HTTP/1.1 4"));
    remember_source(forbidden, "loft malformed", response.local_addr);

    for (label, path) in [
        ("loft health", "/health"),
        ("loft readiness", "/ready"),
        ("loft public info", "/v1/info"),
        ("loft metrics", "/metrics"),
    ] {
        let response = raw_http(
            address,
            DIRECT_CLIENT_IP.parse().unwrap(),
            "GET",
            path,
            &[("X-H21-Disclosure-Selector", DISCLOSURE_SELECTOR)],
            &[],
        )
        .await;
        remember_source(forbidden, label, response.local_addr);
        surfaces.push((label.into(), response.text));
    }
}

async fn drive_directory(
    address: SocketAddr,
    surfaces: &mut Vec<(String, String)>,
    forbidden: &mut Vec<ForbiddenValue>,
) {
    let malformed = format!("{{\"entry\":\"{DISCLOSURE_SELECTOR}\"}}");
    let response = raw_http(
        address,
        TRUSTED_EDGE_IP.parse().unwrap(),
        "POST",
        "/v1/directory/submit",
        &[("X-H21-Disclosure-Selector", DISCLOSURE_SELECTOR)],
        malformed.as_bytes(),
    )
    .await;
    assert!(response.text.starts_with("HTTP/1.1 4"));
    remember_source(forbidden, "directory malformed", response.local_addr);

    for (label, path) in [
        ("directory health", "/health"),
        ("directory public document", "/directory.json"),
        ("directory metrics", "/metrics"),
    ] {
        let response = raw_http(
            address,
            DIRECT_CLIENT_IP.parse().unwrap(),
            "GET",
            path,
            &[("X-H21-Disclosure-Selector", DISCLOSURE_SELECTOR)],
            &[],
        )
        .await;
        remember_source(forbidden, label, response.local_addr);
        surfaces.push((label.into(), response.text));
    }
}

async fn wait_until_ready(process: &mut ServiceProcess, address: SocketAddr, path: &str) {
    let mut last_response = None;
    for _ in 0..160 {
        if process
            .child
            .as_mut()
            .expect("live service child")
            .try_wait()
            .expect("inspect service startup")
            .is_some()
        {
            let child = process.child.take().expect("finished service child");
            let output = child
                .wait_with_output()
                .expect("collect failed service startup output");
            panic!(
                "{} exited before readiness\nstdout:\n{}\nstderr:\n{}",
                process.name,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        if let Ok(response) =
            try_raw_http(address, Ipv4Addr::LOCALHOST.into(), "GET", path, &[], &[]).await
        {
            if response.text.starts_with("HTTP/1.1 200") {
                return;
            }
            last_response = Some(response.text);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "{} did not become ready at {address}{path}; last response:\n{}",
        process.name,
        last_response.as_deref().unwrap_or("<no response>"),
    );
}

async fn raw_http(
    address: SocketAddr,
    source_ip: IpAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> RawResponse {
    try_raw_http(address, source_ip, method, path, headers, body)
        .await
        .unwrap_or_else(|error| panic!("HTTP {method} {path} failed: {error}"))
}

async fn try_raw_http(
    address: SocketAddr,
    source_ip: IpAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<RawResponse> {
    tokio::time::timeout(Duration::from_secs(60), async move {
        let socket = TcpSocket::new_v4()?;
        socket.bind(SocketAddr::new(source_ip, 0))?;
        let stream = socket.connect(address).await?;
        let local_addr = stream.local_addr()?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        )
        .into_bytes();
        for (name, value) in headers {
            request.extend_from_slice(name.as_bytes());
            request.extend_from_slice(b": ");
            request.extend_from_slice(value.as_bytes());
            request.extend_from_slice(b"\r\n");
        }
        if !body.is_empty() {
            request.extend_from_slice(b"Content-Type: application/json\r\n");
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(body);
        write_all(&stream, &request).await?;
        let bytes = read_to_end(&stream, 1024 * 1024).await?;
        Ok(RawResponse {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            local_addr,
        })
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "bounded HTTP exchange timed out"))?
}

async fn write_all(stream: &TcpStream, bytes: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        stream.writable().await?;
        match stream.try_write(&bytes[written..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "socket closed")),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

async fn read_to_end(stream: &TcpStream, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        stream.readable().await?;
        match stream.try_read(&mut chunk) {
            Ok(0) => return Ok(output),
            Ok(count) => {
                if output.len().saturating_add(count) > limit {
                    return Err(io::Error::other("HTTP response exceeded H-21 limit"));
                }
                output.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
}

fn assert_status(label: &str, response: &str, expected: StatusCode) {
    let prefix = format!("HTTP/1.1 {}", expected.as_u16());
    assert!(
        response.starts_with(&prefix),
        "{label} returned an unexpected response: {response}"
    );
}

fn base_forbidden_values() -> Vec<ForbiddenValue> {
    [
        ("forwarded client IP", FORWARDED_CLIENT_IP.to_string()),
        ("forwarded client port", FORWARDED_CLIENT_PORT.to_string()),
        ("raw disclosure selector", DISCLOSURE_SELECTOR.to_string()),
        ("panic-path client IP", PANIC_CLIENT_IP.to_string()),
        ("panic-path client port", PANIC_CLIENT_PORT.to_string()),
    ]
    .into_iter()
    .map(|(label, value)| ForbiddenValue {
        label: label.into(),
        value,
    })
    .collect()
}

fn remember_source(forbidden: &mut Vec<ForbiddenValue>, service: &str, source: SocketAddr) {
    forbidden.push(ForbiddenValue {
        label: format!("{service} client socket"),
        value: source.to_string(),
    });
}

fn privacy_violations(surfaces: &[(String, String)], forbidden: &[ForbiddenValue]) -> Vec<String> {
    let mut violations = Vec::new();
    for (surface, content) in surfaces {
        for item in forbidden {
            if !item.value.is_empty() && content.contains(&item.value) {
                let line = content
                    .lines()
                    .find(|line| line.contains(&item.value))
                    .unwrap_or("<matching output was not line-oriented>");
                let redacted = forbidden.iter().fold(line.to_string(), |line, protected| {
                    line.replace(&protected.value, "<redacted>")
                });
                violations.push(format!(
                    "{surface} disclosed {}; output: {}",
                    item.label,
                    redacted.chars().take(500).collect::<String>()
                ));
            }
        }
    }
    violations
}

fn assert_private_surfaces(surfaces: &[(String, String)], forbidden: &[ForbiddenValue]) {
    let violations = privacy_violations(surfaces, forbidden);
    assert!(
        violations.is_empty(),
        "H-21 privacy invariant failed:\n{}",
        violations.join("\n")
    );
}

fn process_surfaces(name: &str, output: &Output) -> Vec<(String, String)> {
    vec![
        (
            format!("{name} stdout"),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ),
        (
            format!("{name} stderr"),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
    ]
}

fn assert_clean_exit(name: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{name} did not stop cleanly\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn run_error_and_panic_child() -> Output {
    Command::new(std::env::current_exe().expect("current integration-test executable"))
        .args([
            "--exact",
            "h21_internal_error_and_panic_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PIGEONPOST_H21_PANIC_CHILD", "1")
        .env("RUST_BACKTRACE", "full")
        .env("PIGEONPOST_LOG", "trace")
        .output()
        .expect("run H-21 error-and-panic child")
}

fn assert_closed_trace(directory: &Path, label: &str) {
    let has_closed_segment = fs::read_dir(directory)
        .unwrap_or_else(|error| {
            panic!("cannot inspect {label} at {}: {error}", directory.display())
        })
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "pptrace")
        });
    assert!(
        has_closed_segment,
        "{label} did not produce a closed .pptrace segment"
    );
}

fn write_seed(path: &Path, seed: &[u8; 32]) {
    fs::write(path, seed).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
    }
}

fn interrupt(child: &mut Child) {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .expect("invoke kill for graceful test shutdown");
        assert!(status.success(), "SIGINT delivery failed");
    }
    #[cfg(not(unix))]
    {
        child.kill().expect("terminate service process");
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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
