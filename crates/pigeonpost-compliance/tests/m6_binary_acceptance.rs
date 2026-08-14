#![cfg(any(target_os = "linux", target_os = "macos"))]

//! Exact-binary M6 acceptance.
//!
//! This test is ignored during ordinary crate tests because it requires paths to separately built
//! `pigeonpost`, `ppcompliance`, and the test-only adapter example. The checked acceptance script
//! supplies those paths in CI and for every supported native custody release asset.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::SigningKey;
use pigeonpost_compliance::{
    CompletionStatus, DestructionInventory, DisclosureLeaf, DisclosureLedger, InventoryState,
    KeyCopy, KeyCopyKind, RetentionPolicy,
};
use pigeonpost_compliance_format::{
    attribution_epoch_end_ms, ComplianceKeyId, CompliancePurpose, Jurisdiction,
    TRACE_EPOCH_DURATION_MS,
};
use pigeonpost_compliance_seal::{
    epoch_manifest_path, publish_epoch_manifest, EpochManifest, EpochSealingKey, EpochSegmentEntry,
    NetworkOperation, SegmentWriter, TraceIp, TraceRecord,
};
use pigeonpost_core::envelope::{AttributionBlock, Wrap, ENVELOPE_VERSION};
use pigeonpost_registry::entry::ComplianceKeyStatus;
use pigeonpost_registry::{Checkpoint, ComplianceKeyPublish, LogEntry, MerkleLog};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const DAY_MS: u64 = TRACE_EPOCH_DURATION_MS;
const SOURCE_IP_CANARY: &str = "198.51.100.77";
const REGISTRY_ORIGIN: &str = "m6-acceptance.pigeonpost.test/registry";
const WITNESS_NAME: &str = "independent-m6-test-witness";

#[derive(Default)]
struct HttpState {
    info: Vec<u8>,
    compliance: Vec<u8>,
    entries: Vec<u8>,
    dump: Vec<u8>,
    agent_records: HashMap<String, Vec<u8>>,
    wraps: Vec<Wrap>,
    failure: Option<String>,
}

struct HttpFixture {
    url: String,
    state: Arc<Mutex<HttpState>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn start(
        log_path: PathBuf,
        loft_public_key: [u8; 32],
        compliance: Vec<u8>,
        entries: Vec<u8>,
        dump: Vec<u8>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind M6 HTTP fixture");
        listener
            .set_nonblocking(true)
            .expect("make M6 HTTP fixture nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("fixture address"));
        let info = serde_json::to_vec(&json!({
            "software": "pigeonpost-m6-acceptance",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": pigeonpost_core::PROTOCOL_VERSION,
            "pubkey": hex(&loft_public_key),
            "origin": url,
            "capacity_bytes": 1_073_741_824u64,
            "used_bytes": 0u64,
            "utilization": 0.0,
            "retention_days": 1u64,
            "open": true,
            "pow_floor": 0u32,
            "max_event_bytes": 2 * 1024 * 1024,
            "event_count": 0u64,
            "accepting": true,
        }))
        .expect("encode fixture info");
        let state = Arc::new(Mutex::new(HttpState {
            info,
            compliance,
            entries,
            dump,
            ..HttpState::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Err(error) =
                            serve_http_request(&mut stream, &worker_state, &log_path)
                        {
                            worker_state.lock().expect("HTTP fixture state").failure = Some(error);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        worker_state.lock().expect("HTTP fixture state").failure =
                            Some(format!("listener failed: {error}"));
                        break;
                    }
                }
            }
        });
        Self {
            url,
            state,
            stop,
            worker: Some(worker),
        }
    }

    fn only_wrap(&self) -> Wrap {
        let state = self.state.lock().expect("HTTP fixture state");
        assert!(state.failure.is_none(), "HTTP fixture failed");
        assert_eq!(state.wraps.len(), 1, "exactly one wrap must be published");
        state.wraps[0].clone()
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join M6 HTTP fixture");
        }
    }
}

struct RegistryFixture {
    checkpoint: String,
    root: [u8; 32],
    entries_json: Vec<u8>,
    dump: Vec<u8>,
    metadata: Vec<u8>,
    signer: SigningKey,
    witness: SigningKey,
}

#[derive(Default)]
struct Captures(Vec<(String, Vec<u8>, Vec<u8>)>);

impl Captures {
    fn invoke(
        &mut self,
        label: &str,
        binary: &Path,
        args: &[OsString],
        environment: &[(&str, &OsStr)],
        stdin: Option<&[u8]>,
    ) -> Output {
        let mut command = Command::new(binary);
        command.args(args).env_clear();
        for (name, value) in environment {
            command.env(name, value);
        }
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("start exact M6 binary");
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .expect("private request stdin")
                .write_all(input)
                .expect("write private request");
        }
        let output = child.wait_with_output().expect("wait for exact M6 binary");
        self.0.push((
            label.to_owned(),
            output.stdout.clone(),
            output.stderr.clone(),
        ));
        output
    }

    fn success(
        &mut self,
        label: &str,
        binary: &Path,
        args: &[OsString],
        environment: &[(&str, &OsStr)],
        stdin: Option<&[u8]>,
    ) -> Output {
        let output = self.invoke(label, binary, args, environment, stdin);
        // Include what the binary actually said. Asserting on the label alone means a failure here
        // reports only which step broke and none of why, which turns a one-line diagnosis into a
        // CI round trip.
        assert!(
            output.status.success(),
            "{label} failed ({})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        output
    }
}

#[test]
#[ignore = "run through deploy/acceptance/m6-compliance.sh with exact binary paths"]
fn m6_exact_binaries_complete_the_compliance_lifecycle() {
    let pigeonpost = required_executable("PIGEONPOST_BIN");
    let ppcompliance = required_executable("PPCOMPLIANCE_BIN");
    let adapter_source = required_executable("PIGEONPOST_M6_ADAPTER_BIN");
    let temp = tempfile::tempdir().expect("create isolated M6 directory");
    let root = temp.path().canonicalize().expect("canonical M6 directory");
    let proxy_log = root.join("registry-proxy.log");
    write_private(&proxy_log, b"");

    let now_ms = now_ms();
    let trace_start = (now_ms / DAY_MS).saturating_sub(3).saturating_mul(DAY_MS);
    let attribution_start = current_attribution_epoch_start(now_ms);
    let trace_key_id = ComplianceKeyId::new(
        CompliancePurpose::NetworkTrace,
        Jurisdiction::Test,
        [0x41; 32],
        trace_start,
        1,
    );
    let attribution_key_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0x42; 32],
        attribution_start,
        1,
    );
    let trace_secret = [0x31; 32];
    let attribution_secret = [0x32; 32];
    let trace_public = MontgomeryPoint::mul_base_clamped(trace_secret).to_bytes();
    let attribution_public = MontgomeryPoint::mul_base_clamped(attribution_secret).to_bytes();
    let registry = registry_fixture(
        trace_key_id,
        trace_public,
        attribution_key_id,
        attribution_public,
        now_ms,
    );
    let loft_key = SigningKey::from_bytes(&[0x43; 32]);
    let http = HttpFixture::start(
        proxy_log.clone(),
        loft_key.verifying_key().to_bytes(),
        registry.metadata.clone(),
        registry.entries_json.clone(),
        registry.dump.clone(),
    );

    let trust_path = root.join("registry-trust.json");
    let trust = json!({
        "version": 1,
        "registry_url": http.url,
        "origin": REGISTRY_ORIGIN,
        "checkpoint_key": hex(registry.signer.verifying_key().as_bytes()),
        "witnesses": [{
            "name": WITNESS_NAME,
            "public_key": hex(registry.witness.verifying_key().as_bytes()),
        }],
        "witness_threshold": 1,
        "minimum_checkpoint": {
            "size": 0,
            "root": hex(&pigeonpost_registry::log::empty_root()),
        },
        "max_cosignature_age_seconds": 3600,
        "future_clock_skew_seconds": 60,
    });
    write_private(
        &trust_path,
        &serde_json::to_vec(&trust).expect("encode registry trust"),
    );

    let sender_home = root.join("sender");
    let recipient_home = root.join("recipient");
    let mut captures = Captures::default();
    let sender_env = [("PIGEONPOST_HOME", sender_home.as_os_str())];
    let recipient_env = [("PIGEONPOST_HOME", recipient_home.as_os_str())];
    let recipient_id = captures.success(
        "recipient identity",
        &pigeonpost,
        &strings(&["--json", "id"]),
        &recipient_env,
        None,
    );
    let recipient_address = json_stdout(&recipient_id)["address"]
        .as_str()
        .expect("recipient address")
        .to_owned();
    captures.success(
        "recipient loft publication",
        &pigeonpost,
        &[
            OsString::from("loft"),
            OsString::from("add"),
            http.url.clone().into(),
        ],
        &recipient_env,
        None,
    );
    captures.success(
        "sender identity",
        &pigeonpost,
        &strings(&["--json", "id"]),
        &sender_env,
        None,
    );
    captures.success(
        "sender loft publication",
        &pigeonpost,
        &[
            OsString::from("loft"),
            OsString::from("add"),
            http.url.clone().into(),
        ],
        &sender_env,
        None,
    );
    captures.success(
        "sender registry trust",
        &pigeonpost,
        &[
            OsString::from("registry-trust"),
            OsString::from("import"),
            OsString::from("--file"),
            trust_path.as_os_str().to_owned(),
        ],
        &sender_env,
        None,
    );
    captures.success(
        "sender attribution mode",
        &pigeonpost,
        &[
            OsString::from("attribution"),
            OsString::from("sender"),
            OsString::from("test"),
            OsString::from("--authority"),
            OsString::from(hex(&attribution_key_id.authority)),
        ],
        &sender_env,
        None,
    );
    let send = captures.success(
        "attributed send",
        &pigeonpost,
        &[
            OsString::from("--json"),
            OsString::from("send"),
            OsString::from(&recipient_address),
            OsString::from("--body"),
            OsString::from("M6 exact-binary attributed Pigeonpost"),
        ],
        &sender_env,
        None,
    );
    assert_eq!(json_stdout(&send)["delivered"], 1);
    let wrap = http.only_wrap();
    wrap.verify_public()
        .expect("exact binary emitted valid wrap");
    assert_eq!(wrap.version, ENVELOPE_VERSION);
    assert!(matches!(
        wrap.attribution.as_ref(),
        Some(AttributionBlock::V3(block)) if block.key_id == attribution_key_id
    ));

    let offline = root.join("offline");
    let offline_paths = prepare_operator_home(
        &offline,
        &adapter_source,
        &registry,
        trace_key_id,
        trace_secret,
        trace_public,
        attribution_key_id,
        attribution_secret,
        &wrap,
    );
    let compliance_env = [("PIGEONPOST_COMPLIANCE_HOME", offline.as_os_str())];
    for (label, key_id) in [
        ("trace inventory import", trace_key_id),
        ("attribution inventory import", attribution_key_id),
    ] {
        captures.success(
            label,
            &ppcompliance,
            &[
                OsString::from("inventory"),
                OsString::from("import"),
                OsString::from("--epoch"),
                OsString::from(hex(&key_id.encode().expect("key id"))),
            ],
            &compliance_env,
            None,
        );
    }
    captures.success(
        "offline status",
        &ppcompliance,
        &strings(&["status"]),
        &compliance_env,
        None,
    );

    let public_home = root.join("public-only");
    prepare_public_only_home(
        &public_home,
        &adapter_source,
        &registry,
        &offline_paths,
        trace_key_id,
        trace_public,
        attribution_key_id,
        &wrap,
    );
    let trace_selector = format!("event_id={}", hex(&offline_paths.selected_event_id));
    let attribution_selector = format!("event_id={}", hex(&wrap.id()));
    let trace_request = unseal_request(&trace_selector);
    let attribution_request = unseal_request(&attribution_selector);
    let public_env = [("PIGEONPOST_COMPLIANCE_HOME", public_home.as_os_str())];
    let public_trace_attempt = captures.invoke(
        "public-only trace unseal refusal",
        &ppcompliance,
        &[
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(hex(&trace_key_id.encode().expect("trace key id"))),
        ],
        &public_env,
        Some(trace_request.as_bytes()),
    );
    assert!(!public_trace_attempt.status.success());
    assert!(public_trace_attempt.stdout.is_empty());
    let public_attribution_attempt = captures.invoke(
        "public-only attribution unseal refusal",
        &ppcompliance,
        &[
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(hex(&attribution_key_id
                .encode()
                .expect("attribution key id"))),
        ],
        &public_env,
        Some(attribution_request.as_bytes()),
    );
    assert!(!public_attribution_attempt.status.success());
    assert!(public_attribution_attempt.stdout.is_empty());
    let online_help = captures.success(
        "online binary custody boundary",
        &pigeonpost,
        &strings(&["--help"]),
        &[],
        None,
    );
    assert!(!String::from_utf8_lossy(&online_help.stdout).contains("unseal"));

    let trace_unseal = captures.success(
        "authorized trace unseal",
        &ppcompliance,
        &[
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(hex(&trace_key_id.encode().expect("trace key id"))),
        ],
        &compliance_env,
        Some(trace_request.as_bytes()),
    );
    let trace_record: Value = serde_json::from_slice(&trace_unseal.stdout)
        .expect("trace disclosure must be one JSON record");
    assert_eq!(trace_record["kind"], "network_trace");
    assert_eq!(
        trace_record["event_id"],
        hex(&offline_paths.selected_event_id)
    );
    assert_ne!(trace_record["source_address"], SOURCE_IP_CANARY);

    let attribution_unseal = captures.success(
        "authorized attribution unseal",
        &ppcompliance,
        &[
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(hex(&attribution_key_id
                .encode()
                .expect("attribution key id"))),
        ],
        &compliance_env,
        Some(attribution_request.as_bytes()),
    );
    let attribution_record: Value = serde_json::from_slice(&attribution_unseal.stdout)
        .expect("attribution disclosure must be one JSON record");
    assert_eq!(attribution_record["kind"], "attribution");
    assert_eq!(attribution_record["event_id"], hex(&wrap.id()));
    verify_disclosure_pairs(&offline_paths.ledger, &offline_paths.checkpoint_secret, 2);

    let segment_before_hold = fs::read(&offline_paths.trace_segment).expect("read sealed segment");
    let hold_request = b"version = 1\norder_reference = \"M6 preservation fixture\"\n";
    let hold_until = utc_date_after(now_ms, 30);
    let hold = captures.success(
        "place legal hold",
        &ppcompliance,
        &[
            OsString::from("hold"),
            OsString::from("--epoch"),
            OsString::from(hex(&trace_key_id.encode().expect("trace key id"))),
            OsString::from("--until"),
            OsString::from(hold_until),
        ],
        &compliance_env,
        Some(hold_request),
    );
    let hold_id = String::from_utf8(hold.stdout)
        .expect("hold output UTF-8")
        .trim()
        .strip_prefix("hold_id=")
        .expect("hold id output")
        .to_owned();
    let shred_before = utc_date_after(now_ms, 365);
    let held_shred = captures.success(
        "held shred refusal",
        &ppcompliance,
        &[
            OsString::from("shred"),
            OsString::from("--before"),
            OsString::from(&shred_before),
            OsString::from("--execute"),
        ],
        &compliance_env,
        None,
    );
    let held_report = String::from_utf8(held_shred.stdout).expect("held shred output");
    assert!(held_report.contains("shredded_epochs=0"));
    assert!(held_report.contains("skipped_held_epochs=1"));
    assert!(offline_paths.trace_secret.exists());
    assert_eq!(
        fs::read(&offline_paths.trace_segment).expect("read held segment"),
        segment_before_hold
    );
    assert_eq!(
        read_inventory(&offline_paths.trace_inventory).state(),
        InventoryState::Retained
    );

    captures.success(
        "release legal hold",
        &ppcompliance,
        &[
            OsString::from("hold"),
            OsString::from("release"),
            OsString::from("--epoch"),
            OsString::from(hex(&trace_key_id.encode().expect("trace key id"))),
            OsString::from("--hold"),
            OsString::from(&hold_id),
        ],
        &compliance_env,
        Some(hold_request),
    );
    let shredded = captures.success(
        "execute shred",
        &ppcompliance,
        &[
            OsString::from("shred"),
            OsString::from("--before"),
            OsString::from(shred_before),
            OsString::from("--execute"),
        ],
        &compliance_env,
        None,
    );
    let shred_report = String::from_utf8(shredded.stdout).expect("shred output");
    assert!(shred_report.contains("shredded_epochs=1"));
    assert!(!offline_paths.trace_secret.exists());
    assert_eq!(
        read_inventory(&offline_paths.trace_inventory).state(),
        InventoryState::Shredded
    );
    assert_eq!(
        fs::read(&offline_paths.trace_segment).expect("sealed bytes survive crypto-shred"),
        segment_before_hold
    );
    let after_shred = captures.invoke(
        "post-shred unseal refusal",
        &ppcompliance,
        &[
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(hex(&trace_key_id.encode().expect("trace key id"))),
        ],
        &compliance_env,
        Some(trace_request.as_bytes()),
    );
    assert!(!after_shred.status.success());
    assert!(after_shred.stdout.is_empty());

    let checkpoint = captures.success(
        "disclosure checkpoint",
        &ppcompliance,
        &strings(&["checkpoint"]),
        &compliance_env,
        None,
    );
    let published_checkpoint =
        fs::read(&offline_paths.checkpoint_output).expect("published disclosure checkpoint");
    assert_eq!(checkpoint.stdout, published_checkpoint);
    let checkpoint_text = std::str::from_utf8(&checkpoint.stdout).expect("checkpoint UTF-8");
    let verified_checkpoint = Checkpoint::verify(
        checkpoint_text,
        &SigningKey::from_bytes(&offline_paths.checkpoint_secret).verifying_key(),
    )
    .expect("verify disclosure checkpoint");
    assert_eq!(verified_checkpoint.size, 4);

    let forbidden = [
        SOURCE_IP_CANARY.as_bytes(),
        trace_selector.as_bytes(),
        attribution_selector.as_bytes(),
    ];
    for (label, stdout, stderr) in &captures.0 {
        assert_no_canaries(label, stdout, &forbidden);
        assert_no_canaries(label, stderr, &forbidden);
    }
    scan_tree_for_canaries(&root, &forbidden);
}

struct OperatorPaths {
    trace_secret: PathBuf,
    trace_inventory: PathBuf,
    trace_segment: PathBuf,
    attribution_inventory: PathBuf,
    ledger: PathBuf,
    checkpoint_output: PathBuf,
    checkpoint_secret: [u8; 32],
    selected_event_id: [u8; 32],
}

#[allow(clippy::too_many_arguments)]
fn prepare_operator_home(
    home: &Path,
    adapter_source: &Path,
    registry: &RegistryFixture,
    trace_key_id: ComplianceKeyId,
    trace_secret: [u8; 32],
    trace_public: [u8; 32],
    attribution_key_id: ComplianceKeyId,
    attribution_secret: [u8; 32],
    wrap: &Wrap,
) -> OperatorPaths {
    make_private_dir(home);
    let audit_dir = home.join("private-audit");
    let publication_dir = home.join("publication");
    let trace_directory = home.join("trace-epoch");
    make_private_dir(&audit_dir);
    make_private_dir(&publication_dir);
    make_private_dir(&trace_directory);
    let approval_adapter = home.join("approval-adapter");
    let destruction_adapter = home.join("destruction-adapter");
    copy_executable(adapter_source, &approval_adapter);
    copy_executable(adapter_source, &destruction_adapter);

    let trace_secret_path = home.join("trace-custody.secret");
    let attribution_secret_path = home.join("attribution-custody.secret");
    let checkpoint_secret = [0x51; 32];
    let checkpoint_key = home.join("checkpoint.key");
    let audit_key = home.join("private-audit.key");
    write_private(&trace_secret_path, &trace_secret);
    write_private(&attribution_secret_path, &attribution_secret);
    write_private(&checkpoint_key, &checkpoint_secret);
    write_private(&audit_key, &[0x52; 32]);

    let registry_log = home.join("registry.ndjson");
    let registry_checkpoint = home.join("registry.checkpoint");
    write_private(&registry_log, &registry.dump);
    write_private(&registry_checkpoint, registry.checkpoint.as_bytes());

    let selected_event_id = [0x61; 32];
    let segment_path = trace_directory.join(format!(
        "network-{}-00000000.pptrace",
        hex(&trace_key_id.encode().expect("trace key id"))
    ));
    write_closed_trace(
        &trace_directory,
        &segment_path,
        trace_key_id,
        trace_secret,
        trace_public,
        selected_event_id,
    );
    let attribution_wrap = home.join("attributed-wrap.json");
    write_private(
        &attribution_wrap,
        &serde_json::to_vec(wrap).expect("encode attributed wrap"),
    );

    let trace_inventory = home.join("trace.ppinv");
    let trace_import = home.join("trace.import.ppinv");
    let attribution_inventory = home.join("attribution.ppinv");
    let attribution_import = home.join("attribution.import.ppinv");
    let policy = RetentionPolicy::new(365, [0x53; 32]).expect("test retention policy");
    write_private(
        &trace_import,
        &DestructionInventory::new(
            trace_key_id,
            trace_key_id.epoch_start_ms,
            policy,
            all_copies("trace"),
        )
        .expect("trace inventory")
        .encode()
        .expect("encode trace inventory"),
    );
    write_private(
        &attribution_import,
        &DestructionInventory::new(
            attribution_key_id,
            attribution_key_id.epoch_start_ms,
            policy,
            all_copies("attribution"),
        )
        .expect("attribution inventory")
        .encode()
        .expect("encode attribution inventory"),
    );
    let ledger = home.join("disclosure.log");
    let checkpoint_output = publication_dir.join("disclosure.checkpoint");
    let config = operator_config(
        home,
        &ledger,
        &audit_dir,
        &audit_key,
        &checkpoint_key,
        &checkpoint_output,
        &registry_log,
        &registry_checkpoint,
        registry,
        &approval_adapter,
        &destruction_adapter,
        trace_key_id,
        &trace_inventory,
        &trace_import,
        &trace_directory,
        trace_public,
        &trace_secret_path,
        attribution_key_id,
        &attribution_inventory,
        &attribution_import,
        &attribution_wrap,
        &attribution_secret_path,
    );
    write_private(&home.join("config.toml"), config.as_bytes());
    OperatorPaths {
        trace_secret: trace_secret_path,
        trace_inventory,
        trace_segment: segment_path,
        attribution_inventory,
        ledger,
        checkpoint_output,
        checkpoint_secret,
        selected_event_id,
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_public_only_home(
    home: &Path,
    adapter_source: &Path,
    registry: &RegistryFixture,
    offline: &OperatorPaths,
    trace_key_id: ComplianceKeyId,
    trace_public: [u8; 32],
    attribution_key_id: ComplianceKeyId,
    wrap: &Wrap,
) {
    make_private_dir(home);
    let audit_dir = home.join("private-audit");
    let publication_dir = home.join("publication");
    let trace_directory = home.join("trace-epoch");
    make_private_dir(&audit_dir);
    make_private_dir(&publication_dir);
    make_private_dir(&trace_directory);
    let approval_adapter = home.join("approval-adapter");
    let destruction_adapter = home.join("destruction-adapter");
    copy_executable(adapter_source, &approval_adapter);
    copy_executable(adapter_source, &destruction_adapter);
    let trace_segment = trace_directory.join(
        offline
            .trace_segment
            .file_name()
            .expect("trace segment name"),
    );
    copy_private(&offline.trace_segment, &trace_segment);
    let manifest = epoch_manifest_path(
        offline.trace_segment.parent().expect("trace directory"),
        &trace_key_id,
    )
    .expect("trace manifest path");
    let public_manifest =
        epoch_manifest_path(&trace_directory, &trace_key_id).expect("public trace manifest path");
    copy_private(&manifest, &public_manifest);
    let attribution_wrap = home.join("attributed-wrap.json");
    write_private(
        &attribution_wrap,
        &serde_json::to_vec(wrap).expect("encode public attributed wrap"),
    );
    let trace_inventory = home.join("trace.ppinv");
    let attribution_inventory = home.join("attribution.ppinv");
    copy_private(&offline.trace_inventory, &trace_inventory);
    copy_private(&offline.attribution_inventory, &attribution_inventory);
    let registry_log = home.join("registry.ndjson");
    let registry_checkpoint = home.join("registry.checkpoint");
    write_private(&registry_log, &registry.dump);
    write_private(&registry_checkpoint, registry.checkpoint.as_bytes());
    let config = operator_config(
        home,
        &home.join("disclosure.log"),
        &audit_dir,
        &home.join("missing-private-audit.key"),
        &home.join("missing-checkpoint.key"),
        &publication_dir.join("disclosure.checkpoint"),
        &registry_log,
        &registry_checkpoint,
        registry,
        &approval_adapter,
        &destruction_adapter,
        trace_key_id,
        &trace_inventory,
        &home.join("trace.import.ppinv"),
        &trace_directory,
        trace_public,
        &home.join("missing-trace-custody.secret"),
        attribution_key_id,
        &attribution_inventory,
        &home.join("attribution.import.ppinv"),
        &attribution_wrap,
        &home.join("missing-attribution-custody.secret"),
    );
    write_private(&home.join("config.toml"), config.as_bytes());
}

#[allow(clippy::too_many_arguments)]
fn operator_config(
    home: &Path,
    ledger: &Path,
    audit_dir: &Path,
    audit_key: &Path,
    checkpoint_key: &Path,
    checkpoint_output: &Path,
    registry_log: &Path,
    registry_checkpoint: &Path,
    registry: &RegistryFixture,
    approval_adapter: &Path,
    destruction_adapter: &Path,
    trace_key_id: ComplianceKeyId,
    trace_inventory: &Path,
    trace_import: &Path,
    trace_directory: &Path,
    trace_public: [u8; 32],
    trace_secret: &Path,
    attribution_key_id: ComplianceKeyId,
    attribution_inventory: &Path,
    attribution_import: &Path,
    attribution_wrap: &Path,
    attribution_secret: &Path,
) -> String {
    let approval_one = SigningKey::from_bytes(&[21; 32]);
    let approval_two = SigningKey::from_bytes(&[22; 32]);
    let policy_commitment = [0x53; 32];
    format!(
        "version = 2\n\
ledger_path = {ledger:?}\n\
private_audit_directory = {audit_dir:?}\n\
private_audit_key_path = {audit_key:?}\n\
checkpoint_origin = \"m6-acceptance.pigeonpost.test/disclosures\"\n\
checkpoint_signing_key_path = {checkpoint_key:?}\n\
checkpoint_output_path = {checkpoint_output:?}\n\n\
[registry_audit]\n\
log_path = {registry_log:?}\n\
checkpoint_path = {registry_checkpoint:?}\n\
expected_origin = \"{REGISTRY_ORIGIN}\"\n\
checkpoint_key = \"{}\"\n\
witness_threshold = 1\n\
minimum_checkpoint_size = 2\n\
minimum_checkpoint_root = \"{}\"\n\
max_cosignature_age_seconds = 3600\n\
future_clock_skew_seconds = 60\n\n\
[[registry_audit.witnesses]]\n\
name = \"{WITNESS_NAME}\"\n\
public_key = \"{}\"\n\n\
[approval]\n\
request_ttl_ms = 60000\n\n\
[[approval.approvers]]\n\
public_key = \"{}\"\n\
identity = \"m6-officer\"\n\n\
[[approval.approvers]]\n\
public_key = \"{}\"\n\
identity = \"m6-independent-reviewer\"\n\n\
[approval.command]\n\
executable = {approval_adapter:?}\n\
args = []\n\
timeout_ms = 5000\n\n\
[destruction_command]\n\
executable = {destruction_adapter:?}\n\
args = []\n\
timeout_ms = 5000\n\n\
[[epochs]]\n\
key_id = \"{}\"\n\
inventory_path = {trace_inventory:?}\n\
inventory_declaration_path = {:?}\n\
inventory_staging_path = {:?}\n\
inventory_import_path = {trace_import:?}\n\n\
[epochs.retention_policy]\n\
version = 1\n\
tr_days = 365\n\
counsel_approval_commitment = \"{}\"\n\n\
[epochs.artifact]\n\
kind = \"trace_segments\"\n\
expected_node_id = \"{}\"\n\
expected_signer_public_key = \"{}\"\n\
expected_custody_key_digest = \"{}\"\n\
directory = {trace_directory:?}\n\n\
[epochs.custody]\n\
mode = \"software_development\"\n\
secret_key_path = {trace_secret:?}\n\n\
[[epochs]]\n\
key_id = \"{}\"\n\
inventory_path = {attribution_inventory:?}\n\
inventory_declaration_path = {:?}\n\
inventory_staging_path = {:?}\n\
inventory_import_path = {attribution_import:?}\n\n\
[epochs.retention_policy]\n\
version = 1\n\
tr_days = 365\n\
counsel_approval_commitment = \"{}\"\n\n\
[epochs.artifact]\n\
kind = \"attribution_wraps\"\n\
paths = [{attribution_wrap:?}]\n\n\
[epochs.custody]\n\
mode = \"software_development\"\n\
secret_key_path = {attribution_secret:?}\n",
        hex(registry.signer.verifying_key().as_bytes()),
        hex(&registry.root),
        hex(registry.witness.verifying_key().as_bytes()),
        hex(approval_one.verifying_key().as_bytes()),
        hex(approval_two.verifying_key().as_bytes()),
        hex(&trace_key_id.encode().expect("trace key id")),
        home.join("trace-inventory.toml"),
        home.join("trace-staged.ppinv"),
        hex(&policy_commitment),
        hex(&[0x71; 32]),
        hex(SigningKey::from_bytes(&[0x72; 32])
            .verifying_key()
            .as_bytes()),
        hex(&Sha256::digest(trace_public)),
        hex(&attribution_key_id.encode().expect("attribution key id")),
        home.join("attribution-inventory.toml"),
        home.join("attribution-staged.ppinv"),
        hex(&policy_commitment),
    )
}

fn write_closed_trace(
    directory: &Path,
    segment_path: &Path,
    key_id: ComplianceKeyId,
    epoch_secret: [u8; 32],
    custody_public: [u8; 32],
    selected_event_id: [u8; 32],
) {
    let signer = SigningKey::from_bytes(&[0x72; 32]);
    let epoch = EpochSealingKey::from_bytes(key_id, epoch_secret).expect("trace epoch key");
    let mut writer = SegmentWriter::create(
        segment_path,
        epoch,
        &custody_public,
        signer.verifying_key().to_bytes(),
        key_id.epoch_start_ms + 100,
        10,
    )
    .expect("create trace segment");
    for (offset, event_id, source_ip) in [
        (101, selected_event_id, [192, 0, 2, 4]),
        (102, [0x62; 32], [198, 51, 100, 77]),
    ] {
        writer
            .append_network(&TraceRecord {
                jurisdiction: Jurisdiction::Test,
                operation: NetworkOperation::Publish,
                timestamp_ms: key_id.epoch_start_ms + offset,
                node_id: [0x71; 32],
                source_ip: TraceIp::V4(source_ip.into()),
                source_port: 7717,
                event_id: Some(event_id),
                recipient: Some([0x73; 32]),
                owner: None,
                size_bytes: 256,
                correlation_id: None,
            })
            .expect("append sealed trace record");
    }
    let verified = writer
        .finalize(key_id.epoch_start_ms + 200, &signer)
        .expect("close trace segment");
    let manifest = EpochManifest::new_signed(
        key_id,
        [0x71; 32],
        Sha256::digest(custody_public).into(),
        Sha256::digest(epoch_secret).into(),
        vec![EpochSegmentEntry::from_verified(0, &verified).expect("manifest segment")],
        &signer,
    )
    .expect("sign terminal manifest");
    publish_epoch_manifest(
        epoch_manifest_path(directory, &key_id).expect("manifest path"),
        &manifest,
    )
    .expect("publish terminal manifest");
}

fn registry_fixture(
    trace_key_id: ComplianceKeyId,
    trace_public: [u8; 32],
    attribution_key_id: ComplianceKeyId,
    attribution_public: [u8; 32],
    now_ms: u64,
) -> RegistryFixture {
    let signer = SigningKey::from_bytes(&[0x81; 32]);
    let witness = SigningKey::from_bytes(&[0x82; 32]);
    let publications = [
        ComplianceKeyPublish {
            key_id: trace_key_id,
            public_key: hex(&trace_public),
            not_before_ms: trace_key_id.epoch_start_ms,
            not_after_ms: trace_key_id.epoch_start_ms + DAY_MS,
            status: ComplianceKeyStatus::Active,
        },
        ComplianceKeyPublish {
            key_id: attribution_key_id,
            public_key: hex(&attribution_public),
            not_before_ms: attribution_key_id.epoch_start_ms,
            not_after_ms: attribution_epoch_end_ms(&attribution_key_id)
                .expect("canonical attribution epoch"),
            status: ComplianceKeyStatus::Active,
        },
    ];
    let entries: Vec<_> = publications
        .into_iter()
        .enumerate()
        .map(|(index, publication)| {
            LogEntry::compliance_key(index as u64, publication, now_ms + index as u64)
        })
        .collect();
    let mut log = MerkleLog::new();
    for entry in &entries {
        log.append(&entry.leaf_bytes().expect("registry leaf"));
    }
    let root = log.root();
    let checkpoint_value = Checkpoint {
        origin: REGISTRY_ORIGIN.to_owned(),
        size: entries.len() as u64,
        root,
    };
    let mut checkpoint = checkpoint_value.sign(&signer);
    checkpoint.push_str(
        &checkpoint_value
            .cosignature_line(WITNESS_NAME, &witness, now_ms / 1_000)
            .expect("witness registry checkpoint"),
    );
    let mut dump = Vec::new();
    for entry in &entries {
        serde_json::to_writer(&mut dump, entry).expect("encode registry entry");
        dump.push(b'\n');
    }
    let metadata = serde_json::to_vec(&json!({
        "tree_size": entries.len(),
        "root": hex(&root),
        "checkpoint": checkpoint,
        "keys": [],
    }))
    .expect("encode compliance metadata");
    let entries_json = serde_json::to_vec(&json!({
        "from": 0,
        "to": entries.len(),
        "tree_size": entries.len(),
        "root": hex(&root),
        "checkpoint": checkpoint,
        "entries": entries,
    }))
    .expect("encode registry entries");
    RegistryFixture {
        checkpoint,
        root,
        entries_json,
        dump,
        metadata,
        signer,
        witness,
    }
}

fn serve_http_request(
    stream: &mut TcpStream,
    state: &Arc<Mutex<HttpState>>,
    log_path: &Path,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let request = read_http_request(stream)?;
    if request.is_empty() {
        return Ok(());
    }
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| "HTTP fixture received incomplete headers".to_owned())?;
    let headers = std::str::from_utf8(&request[..header_end])
        .map_err(|_| "HTTP fixture received non-UTF-8 headers".to_owned())?;
    let request_line = headers
        .lines()
        .next()
        .ok_or_else(|| "HTTP fixture received no request line".to_owned())?;
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| "HTTP fixture request omitted method".to_owned())?;
    let path = parts
        .next()
        .ok_or_else(|| "HTTP fixture request omitted path".to_owned())?;
    append_private(log_path, format!("{method} {path}\n").as_bytes())?;
    let body = &request[header_end..];

    let (status, content_type, response) = {
        let mut state = state.lock().map_err(|_| "HTTP fixture lock".to_owned())?;
        if method == "GET" && path == "/v1/info" {
            (200, "application/json", state.info.clone())
        } else if method == "PUT" && path.starts_with("/v1/agent/") {
            state.agent_records.insert(path.to_owned(), body.to_vec());
            (204, "application/json", Vec::new())
        } else if method == "GET" && path.starts_with("/v1/agent/") {
            match state.agent_records.get(path) {
                Some(record) => (200, "application/json", record.clone()),
                None => (404, "application/json", Vec::new()),
            }
        } else if method == "POST" && path == "/v1/policy" {
            (204, "application/json", Vec::new())
        } else if method == "POST" && path == "/v1/publish" {
            let value: Value = serde_json::from_slice(body)
                .map_err(|_| "HTTP fixture received malformed publish JSON".to_owned())?;
            let wrap: Wrap = serde_json::from_value(
                value
                    .get("wrap")
                    .cloned()
                    .ok_or_else(|| "HTTP fixture publish omitted wrap".to_owned())?,
            )
            .map_err(|_| "HTTP fixture received malformed wrap".to_owned())?;
            wrap.verify_public()
                .map_err(|_| "HTTP fixture received invalid wrap".to_owned())?;
            let response = serde_json::to_vec(&json!({
                "id": hex(&wrap.id()),
                "stored": true,
            }))
            .map_err(|error| error.to_string())?;
            state.wraps.push(wrap);
            (200, "application/json", response)
        } else if method == "GET" && path.starts_with("/v1/compliance-keys?") {
            (200, "application/json", state.compliance.clone())
        } else if method == "GET" && path.starts_with("/v1/log/dump?") {
            (200, "application/x-ndjson", state.dump.clone())
        } else if method == "GET" && path.starts_with("/v1/log/entries?") {
            (200, "application/json", state.entries.clone())
        } else {
            (404, "application/json", Vec::new())
        }
    };
    write_http_response(stream, status, content_type, &response).map_err(|error| error.to_string())
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut request = Vec::with_capacity(4096);
    let mut content_length = None;
    loop {
        if request.len() > 4 * 1024 * 1024 {
            return Err("HTTP fixture request exceeded its bound".to_owned());
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(header_index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let body_start = header_index + 4;
            let expected = *content_length.get_or_insert_with(|| {
                std::str::from_utf8(&request[..body_start])
                    .ok()
                    .and_then(|headers| {
                        headers.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    })
                    .unwrap_or(0)
            });
            if request.len() >= body_start.saturating_add(expected) {
                request.truncate(body_start + expected);
                break;
            }
        }
    }
    Ok(request)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn all_copies(label: &str) -> Vec<KeyCopy> {
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
        let evidence = format!("m6-{label}-{kind:?}");
        if matches!(kind, KeyCopyKind::LiveMetadata | KeyCopyKind::Backup) {
            KeyCopy::present(kind, evidence.as_bytes()).expect("present custody copy")
        } else {
            KeyCopy::verified_absent(kind, evidence.as_bytes())
                .expect("verified-absent custody copy")
        }
    })
    .collect()
}

fn verify_disclosure_pairs(path: &Path, secret: &[u8; 32], pair_count: usize) {
    let ledger = DisclosureLedger::open(path, secret).expect("open exact-binary disclosure ledger");
    assert_eq!(ledger.leaf_count(), (pair_count * 2) as u64);
    for pair in 0..pair_count {
        let intent = ledger
            .leaf((pair * 2) as u64)
            .expect("read disclosure intent")
            .expect("disclosure intent exists");
        let completion = ledger
            .leaf((pair * 2 + 1) as u64)
            .expect("read disclosure completion")
            .expect("disclosure completion exists");
        match (intent, completion) {
            (DisclosureLeaf::Intent(intent), DisclosureLeaf::Completion(completion)) => {
                assert_eq!(completion.request_id, intent.request_id);
                assert_eq!(completion.status, CompletionStatus::Succeeded);
                assert_eq!(completion.record_count, 1);
            }
            _ => panic!("disclosure leaves are not an intent/completion pair"),
        }
    }
}

fn read_inventory(path: &Path) -> DestructionInventory {
    DestructionInventory::decode(&fs::read(path).expect("read destruction inventory"))
        .expect("decode destruction inventory")
}

fn unseal_request(selector: &str) -> String {
    format!(
        "version = 1\norder_reference = \"M6 test order\"\nrequester_identity = \"M6 test requester\"\nselectors = [{selector:?}]\n"
    )
}

fn current_attribution_epoch_start(now_ms: u64) -> u64 {
    let day_start = now_ms - now_ms % DAY_MS;
    (0..=31)
        .find_map(|days_back| {
            let start = day_start.checked_sub(days_back * DAY_MS)?;
            let key_id = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [0x42; 32],
                start,
                1,
            );
            attribution_epoch_end_ms(&key_id)
                .ok()
                .filter(|end| start <= now_ms && now_ms < *end)
                .map(|_| start)
        })
        .expect("find current canonical attribution epoch")
}

fn utc_date_after(now_ms: u64, days_after: u64) -> String {
    let days = (now_ms / DAY_MS).saturating_add(days_after) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's civil-from-days transform, with day zero at 1970-01-01.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn scan_tree_for_canaries(root: &Path, canaries: &[&[u8]]) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path).expect("scan M6 artifact directory") {
            let entry = entry.expect("read M6 artifact entry");
            let file_type = entry.file_type().expect("read M6 artifact type");
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                let bytes = fs::read(entry.path()).expect("read M6 artifact for privacy scan");
                assert_no_canaries("M6 artifact", &bytes, canaries);
            }
        }
    }
}

fn assert_no_canaries(label: &str, bytes: &[u8], canaries: &[&[u8]]) {
    for canary in canaries {
        assert!(
            !bytes.windows(canary.len()).any(|window| window == *canary),
            "{label} exposed a protected M6 canary"
        );
    }
}

fn required_executable(name: &str) -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")));
    let metadata = fs::metadata(&path).unwrap_or_else(|_| panic!("{name} is not readable"));
    assert!(metadata.is_file(), "{name} must be a regular file");
    assert_ne!(
        metadata.permissions().mode() & 0o111,
        0,
        "{name} must be executable"
    );
    path.canonicalize()
        .unwrap_or_else(|_| panic!("{name} must have a canonical path"))
}

fn make_private_dir(path: &Path) {
    fs::create_dir_all(path).expect("create private M6 directory");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("protect M6 directory");
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    let mut file = options.open(path).expect("create private M6 file");
    file.write_all(bytes).expect("write private M6 file");
    file.sync_all().expect("sync private M6 file");
}

fn append_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())
}

fn copy_private(source: &Path, destination: &Path) {
    let bytes = fs::read(source).expect("read M6 fixture copy source");
    write_private(destination, &bytes);
}

fn copy_executable(source: &Path, destination: &Path) {
    fs::copy(source, destination).expect("copy M6 adapter");
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))
        .expect("protect M6 adapter");
}

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("exact binary stdout must be JSON")
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis()
        .try_into()
        .expect("current time fits u64")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
