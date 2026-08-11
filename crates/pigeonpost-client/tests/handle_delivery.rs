//! Trusted-handle acceptance: registry proof verification feeds ordinary offline-capable delivery.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use pigeonpost_client::Agent;
use pigeonpost_core::{Destination, Identity};
use pigeonpost_loft::{Loft, LoftConfig, LoftStore, SqliteStore};
use pigeonpost_registry::{
    Checkpoint, CheckpointPin, Handle, LogEntry, MerkleLog, RegistryTrust, WitnessKey,
};
use serde_json::{json, Value};

#[tokio::test]
async fn a_witnessed_handle_delivers_and_its_verified_binding_works_offline() {
    let root = tempfile::tempdir().unwrap();
    let loft = spawn_loft(91).await;
    let alice = Agent::open(&root.path().join("alice")).unwrap();
    let bob_home = root.path().join("bob");
    let bob = Agent::open(&bob_home).unwrap();
    // The loopback loft is an explicit local-test fixture, not network-learned routing data.
    // Configuring it on the sender independently authorizes the matching destination hint.
    alice.add_loft(&loft).await.unwrap();
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&alice.verifying_key().to_bytes(), "test setup")
        .unwrap();

    let registry_key = SigningKey::from_bytes(&[71; 32]);
    let witness_a = SigningKey::from_bytes(&[72; 32]);
    let witness_b = SigningKey::from_bytes(&[73; 32]);
    let handle = Handle::parse("/github/bob").unwrap();
    let now = unix_time();
    let (registry_url, registry_task) = spawn_registry(
        &handle,
        bob.verifying_key().to_bytes(),
        &registry_key,
        &witness_a,
        &witness_b,
        now,
    )
    .await;
    let trust = RegistryTrust::new(
        "registry.test/log",
        registry_key.verifying_key().to_bytes(),
        vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("witness-b", witness_b.verifying_key()).unwrap(),
        ],
        2,
        CheckpointPin {
            size: 0,
            root: MerkleLog::new().root(),
        },
        300,
        10,
    )
    .unwrap();
    alice.configure_registry(&registry_url, trust).unwrap();

    let destination = Destination::parse(&format!("/github/bob?l={loft}")).unwrap();
    let first = alice
        .send_to(&destination, "first through a witnessed handle")
        .await
        .unwrap();
    assert_eq!((first.delivered, first.queued), (1, 0));

    // The recipient process can be absent at send time; durable loft storage waits for restart.
    drop(bob);
    let bob = Agent::open(&bob_home).unwrap();
    assert_eq!(bob.drain().await.unwrap().new_messages, 1);

    // Once pinned, temporary registry unavailability uses only the previously verified binding.
    registry_task.abort();
    let second = alice
        .send_to(&destination, "second while the registry is offline")
        .await
        .unwrap();
    assert_eq!((second.delivered, second.queued), (1, 0));
    assert_eq!(bob.drain().await.unwrap().new_messages, 1);
    let bodies: Vec<String> = bob
        .inbox(false, 10)
        .unwrap()
        .into_iter()
        .map(|message| message.body.as_str().to_owned())
        .collect();
    assert!(bodies
        .iter()
        .any(|body| body == "first through a witnessed handle"));
    assert!(bodies
        .iter()
        .any(|body| body == "second while the registry is offline"));
}

async fn spawn_loft(seed: u8) -> String {
    let pubkey = Identity::from_seed([seed; 32]).verifying_key().to_bytes();
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let loft = Arc::new(Loft::new(LoftConfig::new(pubkey, &url), store).unwrap());
    tokio::spawn(async move {
        let _ = pigeonpost_loft::serve(listener, loft, std::future::pending()).await;
    });
    url
}

async fn spawn_registry(
    handle: &Handle,
    pubkey: [u8; 32],
    registry_key: &SigningKey,
    witness_a: &SigningKey,
    witness_b: &SigningKey,
    now: u64,
) -> (String, tokio::task::JoinHandle<()>) {
    let pubkey = hex(&pubkey);
    let entry = LogEntry::handle_claim(
        0,
        handle.as_path(),
        pubkey.clone(),
        "github:test-subject".into(),
        now.saturating_mul(1_000),
    );
    let leaf = entry.leaf_bytes().unwrap();
    let mut log = MerkleLog::new();
    assert_eq!(log.append(&leaf), 0);
    let checkpoint = Checkpoint {
        origin: "registry.test/log".into(),
        size: 1,
        root: log.root(),
    };
    let mut note = checkpoint.sign(registry_key);
    note.push_str(
        &checkpoint
            .cosignature_line("witness-a", witness_a, now)
            .unwrap(),
    );
    note.push_str(
        &checkpoint
            .cosignature_line("witness-b", witness_b, now)
            .unwrap(),
    );
    let resolve = json!({
        "handle": handle.as_path(),
        "pubkey": pubkey,
        "log_index": 0,
        "inclusion_proof": {
            "tree_size": 1,
            "root": hex(&checkpoint.root),
            "path": [],
            "checkpoint": note,
        }
    });
    let entries = json!({
        "from": 0,
        "to": 1,
        "tree_size": 1,
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        "entries": [entry],
    });
    let app = static_registry(resolve, entries);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), task)
}

fn static_registry(resolve: Value, entries: Value) -> Router {
    let resolve = Arc::new(resolve);
    let entries = Arc::new(entries);
    Router::new()
        .route(
            "/v1/resolve/github/bob",
            get({
                let resolve = Arc::clone(&resolve);
                move || {
                    let resolve = Arc::clone(&resolve);
                    async move { Json((*resolve).clone()) }
                }
            }),
        )
        .route(
            "/v1/log/entries",
            get(move || {
                let entries = Arc::clone(&entries);
                async move { Json((*entries).clone()) }
            }),
        )
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
