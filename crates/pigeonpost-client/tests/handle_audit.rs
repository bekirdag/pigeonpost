use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, State as AxumState};
use axum::routing::get;
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use pigeonpost_client::{Agent, ClientError, RegistryTrustBundle, RegistryTrustInput, State};
use pigeonpost_registry::{
    log::empty_root, Checkpoint, CheckpointPin, Handle, LogEntry, MerkleLog, RegistryClient,
    RegistryError, RegistryTrust, WitnessKey,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Notify;

const ORIGIN: &str = "registry.test/handle-audit";
const NOW: u64 = 10_000;

struct Fixture {
    entries: Vec<LogEntry>,
    checkpoints: Vec<Checkpoint>,
    notes: Vec<String>,
    phase: AtomicUsize,
    dump_calls: AtomicUsize,
    range_calls: AtomicUsize,
    pause_dump: AtomicBool,
    dump_started: Notify,
    dump_release: Notify,
}

#[derive(Deserialize)]
struct Range {
    from: u64,
    to: u64,
}

async fn resolve(
    AxumPath((namespace, name)): AxumPath<(String, String)>,
    AxumState(fixture): AxumState<Arc<Fixture>>,
) -> Json<serde_json::Value> {
    let phase = fixture.phase.load(Ordering::SeqCst).min(1);
    let checkpoint = &fixture.checkpoints[phase];
    let handle = format!("/{namespace}/{name}");
    let log_index = match (handle.as_str(), phase) {
        ("/github/alice", 0) => 0,
        ("/github/alice", 1) => 2,
        ("/github/bob", _) => 1,
        _ => panic!("unexpected test handle"),
    };
    let (_, pubkey, _) = fixture.entries[log_index]
        .handle_binding()
        .expect("fixture binding");
    let mut log = MerkleLog::new();
    for entry in &fixture.entries {
        log.append(&entry.leaf_bytes().unwrap());
    }
    Json(json!({
        "handle": handle,
        "pubkey": pubkey,
        "log_index": log_index,
        "inclusion_proof": {
            "tree_size": checkpoint.size,
            "root": hex(&checkpoint.root),
            "path": log.inclusion_proof(log_index as u64, checkpoint.size).unwrap()
                .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
            "checkpoint": fixture.notes[phase],
        }
    }))
}

async fn range_entries(
    Query(range): Query<Range>,
    AxumState(fixture): AxumState<Arc<Fixture>>,
) -> Json<serde_json::Value> {
    fixture.range_calls.fetch_add(1, Ordering::SeqCst);
    let phase = fixture.phase.load(Ordering::SeqCst).min(1);
    let checkpoint = &fixture.checkpoints[phase];
    Json(json!({
        "from": range.from,
        "to": range.to,
        "tree_size": checkpoint.size,
        "root": hex(&checkpoint.root),
        "checkpoint": fixture.notes[phase],
        "entries": fixture.entries[range.from as usize..range.to as usize],
    }))
}

async fn dump(Query(range): Query<Range>, AxumState(fixture): AxumState<Arc<Fixture>>) -> String {
    fixture.dump_calls.fetch_add(1, Ordering::SeqCst);
    if fixture.pause_dump.swap(false, Ordering::SeqCst) {
        fixture.dump_started.notify_one();
        fixture.dump_release.notified().await;
    }
    let mut body = String::new();
    for entry in &fixture.entries[range.from as usize..range.to as usize] {
        body.push_str(&serde_json::to_string(entry).unwrap());
        body.push('\n');
    }
    body
}

async fn consistency(
    Query(range): Query<std::collections::HashMap<String, u64>>,
    AxumState(fixture): AxumState<Arc<Fixture>>,
) -> Json<serde_json::Value> {
    let from = range["from"];
    let checkpoint = &fixture.checkpoints[1];
    let mut log = MerkleLog::new();
    for entry in &fixture.entries {
        log.append(&entry.leaf_bytes().unwrap());
    }
    Json(json!({
        "from": from,
        "to": checkpoint.size,
        "root": hex(&checkpoint.root),
        "path": log.consistency_proof(from, checkpoint.size).unwrap()
            .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
    }))
}

async fn fixture() -> (
    String,
    Arc<Fixture>,
    RegistryTrust,
    tokio::task::JoinHandle<()>,
) {
    let operator = SigningKey::from_bytes(&[101; 32]);
    let witness = SigningKey::from_bytes(&[102; 32]);
    let alice_old = SigningKey::from_bytes(&[103; 32]);
    let bob = SigningKey::from_bytes(&[104; 32]);
    let alice_new = SigningKey::from_bytes(&[105; 32]);
    let entries = vec![
        LogEntry::handle_claim(
            0,
            "/github/alice".into(),
            hex(alice_old.verifying_key().as_bytes()),
            "github:alice-subject".into(),
            9_000_000,
        ),
        LogEntry::handle_claim(
            1,
            "/github/bob".into(),
            hex(bob.verifying_key().as_bytes()),
            "github:bob-subject".into(),
            9_100_000,
        ),
        LogEntry::handle_rotation(
            2,
            "/github/alice".into(),
            hex(alice_new.verifying_key().as_bytes()),
            "github:alice-subject".into(),
            9_200_000,
        ),
    ];
    let mut log = MerkleLog::new();
    let mut roots = Vec::new();
    for entry in &entries {
        log.append(&entry.leaf_bytes().unwrap());
        roots.push(log.root());
    }
    let checkpoints = vec![
        Checkpoint {
            origin: ORIGIN.into(),
            size: 2,
            root: roots[1],
        },
        Checkpoint {
            origin: ORIGIN.into(),
            size: 3,
            root: roots[2],
        },
    ];
    let notes = checkpoints
        .iter()
        .map(|checkpoint| {
            let mut note = checkpoint.sign(&operator);
            note.push_str(
                &checkpoint
                    .cosignature_line("independent", &witness, NOW - 10)
                    .unwrap(),
            );
            note
        })
        .collect();
    let fixture = Arc::new(Fixture {
        entries,
        checkpoints,
        notes,
        phase: AtomicUsize::new(0),
        dump_calls: AtomicUsize::new(0),
        range_calls: AtomicUsize::new(0),
        pause_dump: AtomicBool::new(false),
        dump_started: Notify::new(),
        dump_release: Notify::new(),
    });
    let app = Router::new()
        .route("/v1/resolve/{namespace}/{name}", get(resolve))
        .route("/v1/log/entries", get(range_entries))
        .route("/v1/log/dump", get(dump))
        .route("/v1/log/consistency", get(consistency))
        .with_state(Arc::clone(&fixture));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let trust = RegistryTrust::new(
        ORIGIN,
        operator.verifying_key().to_bytes(),
        vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
        1,
        CheckpointPin {
            size: 0,
            root: empty_root(),
        },
        60,
        5,
    )
    .unwrap();
    (format!("http://{address}"), fixture, trust, server)
}

fn import_trust(home: &std::path::Path, url: &str, trust: &RegistryTrust) {
    let agent = Agent::open(home).unwrap();
    let bundle = RegistryTrustBundle::from_registry_trust(url, trust).unwrap();
    let input = RegistryTrustInput::from_json(&serde_json::to_vec(&bundle).unwrap()).unwrap();
    agent.import_registry_trust(input).unwrap();
}

#[tokio::test]
async fn one_bootstrap_serves_two_handles_then_incrementally_survives_restart_and_tamper() {
    let (url, fixture, trust, server) = fixture().await;
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    import_trust(&home, &url, &trust);
    let path = home.join("state.db");
    let state = State::open(&path).unwrap();
    let client = RegistryClient::new(&url, trust.clone()).unwrap();

    let alice = Handle::parse("/github/alice").unwrap();
    let bob = Handle::parse("/github/bob").unwrap();
    state
        .resolve_handle_audited(&client, &alice, NOW)
        .await
        .unwrap();
    assert_eq!(fixture.dump_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.range_calls.load(Ordering::SeqCst), 1);

    state
        .resolve_handle_audited(&client, &bob, NOW)
        .await
        .unwrap();
    assert_eq!(fixture.dump_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.range_calls.load(Ordering::SeqCst),
        2,
        "the second new handle needs only its exact inclusion leaf, not another prefix replay"
    );
    assert_eq!(state.handle_audit().unwrap().unwrap().size(), 2);
    let (_, alice_old, _) = fixture.entries[0].handle_binding().unwrap();
    let (_, bob_key, _) = fixture.entries[1].handle_binding().unwrap();
    let (_, alice_new, _) = fixture.entries[2].handle_binding().unwrap();
    let alice_old = decode_hex32(alice_old);
    let bob_key = decode_hex32(bob_key);
    let alice_new = decode_hex32(alice_new);
    assert!(state.has_current_verified_handle(&alice_old, NOW).unwrap());
    assert!(state.has_current_verified_handle(&bob_key, NOW).unwrap());
    assert!(!state.has_current_verified_handle(&alice_new, NOW).unwrap());

    // `PRAGMA data_version` must invalidate a warm cryptographic cache when another process
    // mutates the projection. The indexed row is still validated at the point of use.
    let external = rusqlite::Connection::open(&path).unwrap();
    external
        .execute(
            "UPDATE registry_handle_projection SET subject = ''
             WHERE handle = '/github/alice'",
            [],
        )
        .unwrap();
    drop(external);
    assert!(state.has_current_verified_handle(&alice_old, NOW).is_err());
    let external = rusqlite::Connection::open(&path).unwrap();
    external
        .execute(
            "UPDATE registry_handle_projection SET subject = 'github:alice-subject'
             WHERE handle = '/github/alice'",
            [],
        )
        .unwrap();
    drop(external);
    assert!(state.has_current_verified_handle(&alice_old, NOW).unwrap());
    assert!(
        !state
            .has_current_verified_handle(&alice_old, NOW + 61)
            .unwrap(),
        "expired witness evidence must withhold the tier without network lookup"
    );
    drop(state);

    fixture.phase.store(1, Ordering::SeqCst);
    let state = State::open(&path).unwrap();
    let (verified, _) = state
        .resolve_handle_audited(&client, &alice, NOW)
        .await
        .unwrap();
    assert_eq!(verified.log_index(), 2);
    assert_eq!(
        fixture.dump_calls.load(Ordering::SeqCst),
        2,
        "the unseen suffix uses the same exact segmented stream engine as bootstrap"
    );
    assert_eq!(fixture.range_calls.load(Ordering::SeqCst), 3);
    assert_eq!(state.handle_audit().unwrap().unwrap().size(), 3);
    assert!(
        !state.has_current_verified_handle(&alice_old, NOW).unwrap(),
        "a rotated-away key must immediately lose the handle tier"
    );
    assert!(state.has_current_verified_handle(&alice_new, NOW).unwrap());
    assert!(state.has_current_verified_handle(&bob_key, NOW).unwrap());
    drop(state);

    // A same-index changed projection cannot be silently trusted after restart.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE registry_handle_projection SET pubkey = ?1 WHERE handle = '/github/alice'",
        [vec![0x55; 32]],
    )
    .unwrap();
    drop(conn);
    let state = State::open(&path).unwrap();
    let error = state
        .resolve_handle_audited(&client, &alice, NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Registry(RegistryError::MalformedEntry(_))
    ));
    drop(state);

    // Restore the normalized row, then prove the cache pin independently rejects a decreasing
    // log index even though the witnessed checkpoint itself is unchanged and valid.
    let (_, alice_key, _) = fixture.entries[2].handle_binding().unwrap();
    let alice_key = decode_hex32(alice_key);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE registry_handle_projection SET pubkey = ?1 WHERE handle = '/github/alice'",
        [alice_key.to_vec()],
    )
    .unwrap();
    conn.execute(
        "UPDATE handle_resolutions
         SET log_index = 2, checkpoint_size = 3 WHERE handle = '/github/bob'",
        [],
    )
    .unwrap();
    drop(conn);
    let state = State::open(&path).unwrap();
    let error = state
        .resolve_handle_audited(&client, &bob, NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Core(pigeonpost_core::Error::StaleSequence)
    ));
    drop(state);

    // A different valid key/address at the same historical index is equivocation too, even if an
    // attacker leaves the checkpoint size untouched.
    let tampered = SigningKey::from_bytes(&[106; 32]);
    let tampered_address = pigeonpost_core::Address::from_pubkey(&tampered.verifying_key());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE handle_resolutions
         SET address = ?1, pubkey = ?2, log_index = 1, checkpoint_size = 3
         WHERE handle = '/github/bob'",
        rusqlite::params![
            tampered_address.as_str(),
            tampered.verifying_key().as_bytes().as_slice(),
        ],
    )
    .unwrap();
    drop(conn);
    let state = State::open(&path).unwrap();
    let error = state
        .resolve_handle_audited(&client, &bob, NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Core(pigeonpost_core::Error::StaleSequence)
    ));
    server.abort();
}

#[tokio::test]
async fn handle_tier_withholds_on_missing_or_lagged_evidence_and_errors_on_tamper() {
    let (url, fixture, trust, server) = fixture().await;
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    import_trust(&home, &url, &trust);
    let path = home.join("state.db");
    let state = State::open(&path).unwrap();
    let client = RegistryClient::new(&url, trust.clone()).unwrap();
    let alice = Handle::parse("/github/alice").unwrap();
    let (_, alice_key, _) = fixture.entries[0].handle_binding().unwrap();
    let alice_key = decode_hex32(alice_key);

    assert!(
        !state.has_current_verified_handle(&alice_key, NOW).unwrap(),
        "configured trust without a complete local projection is not a handle tier"
    );
    state
        .resolve_handle_audited(&client, &alice, NOW)
        .await
        .unwrap();
    assert!(state.has_current_verified_handle(&alice_key, NOW).unwrap());

    // Another registry consumer may advance the sole pin first. Until the complete handle audit
    // catches up to that exact checkpoint, credit is withheld rather than inferred from a prefix.
    state
        .save_registry_checkpoint(&fixture.checkpoints[1], Some(NOW - 10))
        .unwrap();
    assert!(!state.has_current_verified_handle(&alice_key, NOW).unwrap());
    drop(state);

    // Restore an independently bootstrapped exact snapshot in another home, then corrupt the
    // signed note. Static authentication errors must surface instead of looking like mere expiry.
    let tampered_home_root = tempfile::tempdir().unwrap();
    let tampered_home = tampered_home_root.path().join("agent");
    import_trust(&tampered_home, &url, &trust);
    let tampered_path = tampered_home.join("state.db");
    let tampered_state = State::open(&tampered_path).unwrap();
    tampered_state
        .resolve_handle_audited(&client, &alice, NOW)
        .await
        .unwrap();
    drop(tampered_state);
    let conn = rusqlite::Connection::open(&tampered_path).unwrap();
    let encoded: Vec<u8> = conn
        .query_row(
            "SELECT state FROM registry_handle_audit WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["checkpoint_note"] = json!("not a signed checkpoint note");
    conn.execute(
        "UPDATE registry_handle_audit SET state = ?1 WHERE id = 1",
        [serde_json::to_vec(&value).unwrap()],
    )
    .unwrap();
    drop(conn);
    let tampered_state = State::open(&tampered_path).unwrap();
    assert!(tampered_state
        .has_current_verified_handle(&alice_key, NOW)
        .is_err());
    server.abort();
}

#[tokio::test]
async fn slow_registry_stream_does_not_hold_the_agent_database_writer() {
    const WATCHDOG: Duration = Duration::from_secs(10);

    let (url, fixture, trust, server) = fixture().await;
    fixture.pause_dump.store(true, Ordering::SeqCst);
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    import_trust(&home, &url, &trust);
    let path = home.join("state.db");
    let resolving = State::open(&path).unwrap();
    let concurrent = State::open(&path).unwrap();
    let client = RegistryClient::new(&url, trust).unwrap();
    let alice = Handle::parse("/github/alice").unwrap();
    let resolution = resolving.resolve_handle_audited(&client, &alice, NOW);
    tokio::pin!(resolution);
    let reached_pause = tokio::time::timeout(WATCHDOG, async {
        tokio::select! {
            () = fixture.dump_started.notified() => true,
            _ = &mut resolution => false,
        }
    })
    .await;
    match reached_pause {
        Ok(true) => {}
        Ok(false) => {
            fixture.dump_release.notify_one();
            server.abort();
            panic!("handle audit completed before the registry dump paused");
        }
        Err(_) => {
            fixture.dump_release.notify_one();
            server.abort();
            panic!("registry dump did not reach its controlled pause");
        }
    }

    let mut writer = tokio::task::spawn_blocking(move || {
        concurrent
            .set_meta("concurrent_probe", "ok")
            .map_err(|error| error.to_string())
    });
    match tokio::time::timeout(WATCHDOG, &mut writer).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            fixture.dump_release.notify_one();
            let _ = tokio::time::timeout(WATCHDOG, &mut resolution).await;
            server.abort();
            panic!("concurrent writer failed before stream release: {error}");
        }
        Ok(Err(error)) => {
            fixture.dump_release.notify_one();
            let _ = tokio::time::timeout(WATCHDOG, &mut resolution).await;
            server.abort();
            panic!("concurrent writer task failed before stream release: {error}");
        }
        Err(_) => {
            fixture.dump_release.notify_one();
            let _ = tokio::time::timeout(WATCHDOG, async {
                let _ = tokio::join!(&mut resolution, &mut writer);
            })
            .await;
            server.abort();
            panic!("agent database writer remained blocked while the registry stream was paused");
        }
    }

    fixture.dump_release.notify_one();
    let resolved = tokio::time::timeout(WATCHDOG, &mut resolution).await;
    server.abort();
    resolved
        .expect("handle audit did not finish after the dump was released")
        .unwrap();
    assert_eq!(
        resolving.get_meta("concurrent_probe").unwrap().as_deref(),
        Some("ok")
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex32(value: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap();
    }
    out
}
