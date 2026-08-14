//! C2SP witness-client interoperability and adversarial recovery tests.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
#[cfg(unix)]
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
#[cfg(unix)]
use pigeonpost_registry::entry::{directory_add_claim_payload, DirectoryAdd};
use pigeonpost_registry::log::{verify_consistency, Hash};
use pigeonpost_registry::{
    Checkpoint, MerkleLog, WitnessClient, WitnessConfig, WitnessError, WitnessTiming,
};
#[cfg(unix)]
use pigeonpost_registry::{
    Registry, RegistryConfig, RegistryError, WitnessPolicy, WitnessSupervisor,
};
#[cfg(unix)]
use rusqlite::Connection;

const ORIGIN: &str = "pigeonpost.test/registry";
const WITNESS_NAME: &str = "witness.test/one";
const NOW: u64 = 10_000;

#[derive(Debug, Clone, Copy)]
enum Mode {
    Normal,
    Stale,
    Future,
    Tampered,
    Unknown,
    MissingNewline,
    Oversized,
    ConflictPreload,
    Redirect,
}

struct FakeInner {
    mode: Mode,
    failures_left: usize,
    latest_note: Option<String>,
    requests: Vec<Vec<u8>>,
}

struct FakeWitness {
    operator: SigningKey,
    witness: SigningKey,
    inner: Mutex<FakeInner>,
    cosignature_time: AtomicU64,
    pause_next_submission: AtomicBool,
    submission_paused: tokio::sync::Notify,
    release_submission: tokio::sync::Notify,
}

impl FakeWitness {
    fn new() -> Self {
        Self {
            operator: SigningKey::from_bytes(&[11; 32]),
            witness: SigningKey::from_bytes(&[22; 32]),
            inner: Mutex::new(FakeInner {
                mode: Mode::Normal,
                failures_left: 0,
                latest_note: None,
                requests: Vec::new(),
            }),
            cosignature_time: AtomicU64::new(NOW),
            pause_next_submission: AtomicBool::new(false),
            submission_paused: tokio::sync::Notify::new(),
            release_submission: tokio::sync::Notify::new(),
        }
    }

    fn set_mode(&self, mode: Mode) {
        self.inner.lock().unwrap().mode = mode;
    }

    fn fail_next(&self, count: usize) {
        self.inner.lock().unwrap().failures_left = count;
    }

    fn requests(&self) -> Vec<Vec<u8>> {
        self.inner.lock().unwrap().requests.clone()
    }

    fn set_latest(&self, note: String) {
        self.inner.lock().unwrap().latest_note = Some(note);
    }

    #[cfg(unix)]
    fn set_cosignature_time(&self, now_secs: u64) {
        self.cosignature_time.store(now_secs, Ordering::SeqCst);
    }

    #[cfg(unix)]
    fn pause_next_submission(&self) {
        assert!(
            !self.pause_next_submission.swap(true, Ordering::SeqCst),
            "a witness submission is already scheduled to pause"
        );
    }

    #[cfg(unix)]
    async fn wait_until_submission_paused(&self) {
        self.submission_paused.notified().await;
    }

    #[cfg(unix)]
    fn release_submission(&self) {
        self.release_submission.notify_one();
    }
}

async fn add_checkpoint(
    State(state): State<Arc<FakeWitness>>,
    body: Bytes,
) -> Response<axum::body::Body> {
    let Ok(text) = std::str::from_utf8(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some((proof_text, operator_note)) = text.split_once("\n\n") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut lines = proof_text.lines();
    let Some(old_size) = lines
        .next()
        .and_then(|line| line.strip_prefix("old "))
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(proof) = lines.map(decode_hash).collect::<Option<Vec<_>>>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(target) = Checkpoint::verify(operator_note, &state.operator.verifying_key()) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if target.origin != ORIGIN || proof.len() > 63 || (old_size == 0 && !proof.is_empty()) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }

    if state.pause_next_submission.swap(false, Ordering::SeqCst) {
        state.submission_paused.notify_one();
        state.release_submission.notified().await;
    }

    let now_secs = state.cosignature_time.load(Ordering::SeqCst);
    let mut inner = state.inner.lock().unwrap();
    inner.requests.push(body.to_vec());
    if inner.failures_left > 0 {
        inner.failures_left -= 1;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if matches!(inner.mode, Mode::Redirect) {
        let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
        response.headers_mut().insert(
            LOCATION,
            HeaderValue::from_static("http://127.0.0.1:9/forbidden"),
        );
        return response;
    }

    let latest = inner
        .latest_note
        .as_deref()
        .and_then(|note| Checkpoint::verify(note, &state.operator.verifying_key()).ok());
    if matches!(inner.mode, Mode::ConflictPreload) {
        let line = target
            .cosignature_line(WITNESS_NAME, &state.witness, now_secs)
            .unwrap();
        inner.latest_note = Some(format!("{operator_note}{line}"));
        inner.mode = Mode::Normal;
        return conflict(target.size);
    }
    let expected_old = latest.as_ref().map_or(0, |checkpoint| checkpoint.size);
    if old_size != expected_old {
        return conflict(expected_old);
    }
    let consistent = match latest.as_ref() {
        None => old_size == 0 && proof.is_empty(),
        Some(old) if old.size == 0 => proof.is_empty(),
        Some(old) if old.size == target.size => proof.is_empty() && old.root == target.root,
        Some(old) => verify_consistency(old.size, &old.root, target.size, &target.root, &proof),
    };
    if !consistent {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }

    let response = match inner.mode {
        Mode::Normal => target
            .cosignature_line(WITNESS_NAME, &state.witness, now_secs)
            .unwrap(),
        Mode::Stale => target
            .cosignature_line(WITNESS_NAME, &state.witness, now_secs.saturating_sub(500))
            .unwrap(),
        Mode::Future => target
            .cosignature_line(WITNESS_NAME, &state.witness, now_secs.saturating_add(500))
            .unwrap(),
        Mode::Tampered => {
            let mut line = target
                .cosignature_line(WITNESS_NAME, &state.witness, now_secs)
                .unwrap();
            let position = line.len() - 3;
            let replacement = if &line[position..position + 1] == "A" {
                "B"
            } else {
                "A"
            };
            line.replace_range(position..position + 1, replacement);
            line
        }
        Mode::Unknown => target
            .cosignature_line(
                "unknown.test/witness",
                &SigningKey::from_bytes(&[99; 32]),
                now_secs,
            )
            .unwrap(),
        Mode::MissingNewline => target
            .cosignature_line(WITNESS_NAME, &state.witness, now_secs)
            .unwrap()
            .trim_end_matches('\n')
            .to_owned(),
        Mode::Oversized => "x".repeat(70 * 1024),
        Mode::ConflictPreload | Mode::Redirect => unreachable!(),
    };
    if matches!(inner.mode, Mode::Normal) {
        inner.latest_note = Some(format!("{operator_note}{response}"));
    }
    (StatusCode::OK, response).into_response()
}

async fn monitored_checkpoint(
    State(state): State<Arc<FakeWitness>>,
    Path(_origin_hash): Path<String>,
) -> Response<axum::body::Body> {
    state
        .inner
        .lock()
        .unwrap()
        .latest_note
        .clone()
        .map(|note| (StatusCode::OK, note).into_response())
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

fn conflict(size: u64) -> Response<axum::body::Body> {
    let mut response = (StatusCode::CONFLICT, format!("{size}\n")).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/x.tlog.size"));
    response
}

async fn start_witness() -> (Arc<FakeWitness>, WitnessClient) {
    let state = Arc::new(FakeWitness::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/submission/add-checkpoint", post(add_checkpoint))
        .route(
            "/monitoring/{origin_hash}/checkpoint",
            get(monitored_checkpoint),
        )
        .with_state(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = WitnessConfig::new(
        WITNESS_NAME,
        state.witness.verifying_key(),
        &format!("http://{address}/submission"),
        &format!("http://{address}/monitoring"),
        ORIGIN,
    )
    .unwrap();
    let client = WitnessClient::new(
        config,
        ORIGIN,
        state.operator.verifying_key(),
        WitnessTiming {
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            max_cosignature_age: Duration::from_secs(120),
            future_clock_skew: Duration::from_secs(10),
            retry_initial: Duration::from_millis(10),
            retry_max: Duration::from_millis(20),
            retry_deadline: Duration::from_secs(2),
        },
    )
    .unwrap();
    (state, client)
}

fn signed_checkpoint(log: &MerkleLog, operator: &SigningKey) -> String {
    Checkpoint {
        origin: ORIGIN.into(),
        size: log.size(),
        root: log.root(),
    }
    .sign(operator)
}

#[tokio::test]
async fn initial_and_growing_checkpoints_use_exact_c2sp_requests() {
    let (state, client) = start_witness().await;
    let mut log = MerkleLog::new();
    log.append(b"one");
    let first_note = signed_checkpoint(&log, &state.operator);
    let first = client
        .cosign_with(&first_note, None, NOW, |_, _| panic!("no genesis proof"))
        .await
        .unwrap();
    let requests = state.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        std::str::from_utf8(&requests[0]).unwrap(),
        format!("old 0\n\n{first_note}")
    );

    log.append(b"two");
    log.append(b"three");
    let next_note = signed_checkpoint(&log, &state.operator);
    let next = client
        .cosign_with(&next_note, Some(&first), NOW, |old, new| {
            assert_eq!((old, new), (1, 3));
            Ok(log.consistency_proof(old, new).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(next.size(), 3);
    assert_eq!(next.witnessed_at(), NOW);
    let verified = Checkpoint::verify_with_fresh_witnesses(
        next.note(),
        &state.operator.verifying_key(),
        &[client.witness_key()],
        1,
        NOW,
        120,
        10,
    )
    .unwrap();
    assert_eq!(
        verified.checkpoint,
        Checkpoint {
            origin: ORIGIN.into(),
            size: 3,
            root: log.root()
        }
    );
}

#[tokio::test]
async fn stale_future_tampered_and_unknown_cosignatures_are_rejected() {
    for mode in [
        Mode::Stale,
        Mode::Future,
        Mode::Tampered,
        Mode::Unknown,
        Mode::MissingNewline,
    ] {
        let (state, client) = start_witness().await;
        state.set_mode(mode);
        let mut log = MerkleLog::new();
        log.append(b"entry");
        let note = signed_checkpoint(&log, &state.operator);
        assert_eq!(
            client
                .cosign_with(&note, None, NOW, |_, _| Ok(Vec::new()))
                .await,
            Err(WitnessError::InvalidCosignature)
        );
    }
}

#[tokio::test]
async fn local_rollback_and_equivocation_are_stopped_before_submission() {
    let (state, client) = start_witness().await;
    let mut log = MerkleLog::new();
    log.append(b"one");
    let note = signed_checkpoint(&log, &state.operator);
    let receipt = client
        .cosign_with(&note, None, NOW, |_, _| Ok(Vec::new()))
        .await
        .unwrap();
    let request_count = state.requests().len();

    let empty = signed_checkpoint(&MerkleLog::new(), &state.operator);
    assert_eq!(
        client
            .cosign_with(&empty, Some(&receipt), NOW, |_, _| Ok(Vec::new()))
            .await,
        Err(WitnessError::Rollback)
    );
    let mut fork = MerkleLog::new();
    fork.append(b"different");
    let fork_note = signed_checkpoint(&fork, &state.operator);
    assert_eq!(
        client
            .cosign_with(&fork_note, Some(&receipt), NOW, |_, _| Ok(Vec::new()))
            .await,
        Err(WitnessError::Equivocation)
    );
    assert_eq!(state.requests().len(), request_count);
}

#[tokio::test]
async fn conflict_recovery_and_persisted_receipts_survive_restart() {
    let (state, client) = start_witness().await;
    state.set_mode(Mode::ConflictPreload);
    let mut log = MerkleLog::new();
    log.append(b"one");
    let first_note = signed_checkpoint(&log, &state.operator);
    let first = client
        .cosign_with(&first_note, None, NOW, |_, _| Ok(Vec::new()))
        .await
        .unwrap();
    assert_eq!(first.size(), 1);

    let persisted = serde_json::to_vec(&first).unwrap();
    let restored = serde_json::from_slice(&persisted).unwrap();
    log.append(b"two");
    let second_note = signed_checkpoint(&log, &state.operator);
    let second = client
        .cosign_with(&second_note, Some(&restored), NOW, |old, new| {
            Ok(log.consistency_proof(old, new).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(second.size(), 2);

    // Simulate a crash after the witness persisted size three but before the registry persisted
    // the receipt. The next process starts from size two, receives 409, and safely retries at the
    // witness-reported size without waiting for the optional monitoring mirror.
    log.append(b"three");
    let third_note = signed_checkpoint(&log, &state.operator);
    state.set_mode(Mode::ConflictPreload);
    let third = client
        .cosign_with(&third_note, Some(&second), NOW, |old, new| {
            Ok(log.consistency_proof(old, new).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(third.size(), 3);
}

#[tokio::test]
async fn monitored_equivocation_and_witness_ahead_are_rejected() {
    let (state, client) = start_witness().await;
    let mut honest = MerkleLog::new();
    honest.append(b"one");
    let first_note = signed_checkpoint(&honest, &state.operator);
    let first = client
        .cosign_with(&first_note, None, NOW, |_, _| Ok(Vec::new()))
        .await
        .unwrap();

    let mut fork = MerkleLog::new();
    fork.append(b"different");
    fork.append(b"second");
    let fork_note = signed_checkpoint(&fork, &state.operator);
    let fork_checkpoint = Checkpoint::verify(&fork_note, &state.operator.verifying_key()).unwrap();
    let fork_line = fork_checkpoint
        .cosignature_line(WITNESS_NAME, &state.witness, NOW)
        .unwrap();
    state.set_latest(format!("{fork_note}{fork_line}"));

    honest.append(b"two");
    let honest_note = signed_checkpoint(&honest, &state.operator);
    assert_eq!(
        client
            .cosign_with(&honest_note, Some(&first), NOW, |old, new| {
                Ok(honest.consistency_proof(old, new).unwrap())
            })
            .await,
        Err(WitnessError::Equivocation)
    );

    fork.append(b"third");
    let ahead_note = signed_checkpoint(&fork, &state.operator);
    let ahead_checkpoint =
        Checkpoint::verify(&ahead_note, &state.operator.verifying_key()).unwrap();
    let ahead_line = ahead_checkpoint
        .cosignature_line(WITNESS_NAME, &state.witness, NOW)
        .unwrap();
    state.set_latest(format!("{ahead_note}{ahead_line}"));
    assert_eq!(
        client
            .cosign_with(&honest_note, Some(&first), NOW, |old, new| {
                Ok(honest.consistency_proof(old, new).unwrap())
            })
            .await,
        Err(WitnessError::WitnessAhead)
    );
}

#[tokio::test]
async fn responses_redirects_and_retries_remain_bounded() {
    let (state, client) = start_witness().await;
    let mut log = MerkleLog::new();
    log.append(b"one");
    let note = signed_checkpoint(&log, &state.operator);

    state.set_mode(Mode::Oversized);
    assert_eq!(
        client
            .cosign_with(&note, None, NOW, |_, _| Ok(Vec::new()))
            .await,
        Err(WitnessError::ResponseTooLarge)
    );
    state.set_mode(Mode::Redirect);
    assert_eq!(
        client
            .cosign_with(&note, None, NOW, |_, _| Ok(Vec::new()))
            .await,
        Err(WitnessError::Rejected)
    );

    state.set_mode(Mode::Normal);
    state.fail_next(2);
    let receipt = client
        .cosign_with_retry(&note, None, NOW, |_, _| Ok(Vec::new()))
        .await
        .unwrap();
    assert_eq!(receipt.size(), 1);
    assert!(state.requests().len() >= 4);
}

#[tokio::test]
async fn debug_and_error_text_never_disclose_endpoints() {
    let (_state, client) = start_witness().await;
    let debug = format!("{client:?}");
    assert!(debug.contains("<withheld>"));
    assert!(!debug.contains("127.0.0.1"));
    let error = WitnessError::TransportUnavailable.to_string();
    assert!(!error.contains("http"));
    assert!(!error.contains("127.0.0.1"));
}

#[cfg(unix)]
#[tokio::test]
async fn supervisor_retries_when_a_complete_sync_races_with_an_append() {
    let (state, client) = start_witness().await;
    let now_secs = system_now_secs();
    state.set_cosignature_time(now_secs);
    state.pause_next_submission();

    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(directory.path()).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(directory.path(), permissions).unwrap();
    }
    let database = directory.path().join("registry.db");
    let policy = WitnessPolicy::new(vec![client.witness_key()], 1, 120, 10, 0).unwrap();
    let registry = Arc::new(
        Registry::open(
            database.to_str().unwrap(),
            RegistryConfig {
                origin: ORIGIN.into(),
                signing_key: state.operator.clone(),
                allow_mock_identities: true,
            },
        )
        .unwrap()
        .with_witness_policy(policy)
        .unwrap(),
    );
    let supervisor = Arc::new(
        WitnessSupervisor::new(
            Arc::clone(&registry),
            vec![client],
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let run = tokio::spawn({
        let supervisor = Arc::clone(&supervisor);
        async move { supervisor.run(shutdown_rx).await }
    });

    tokio::time::timeout(
        Duration::from_secs(60),
        state.wait_until_submission_paused(),
    )
    .await
    .expect("the first witness submission should pause");

    // The supervisor captured the empty head before the pause. Commit a new head so its
    // successful empty-head sync is immediately followed by an unready zero-lag check.
    let mutation = signed_directory_add(&SigningKey::from_bytes(&[55; 32]), 1);
    let pending = registry.append_directory_add(mutation).unwrap();
    assert!(pending.appended);
    assert_eq!(registry.committed_size().unwrap(), 1);
    assert_eq!(registry.size().unwrap(), 0);
    state.release_submission();

    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if registry
                .witness_publication_status()
                .unwrap()
                .published_size
                == 1
            {
                break;
            }
            assert!(
                !run.is_finished(),
                "the supervisor exited after syncing the stale snapshot"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the supervisor should catch up to the newly committed head");

    assert!(!run.is_finished());
    let publication = registry.witness_publication_status().unwrap();
    assert_eq!(publication.committed_size, 1);
    assert_eq!(publication.published_size, 1);
    assert_eq!(publication.lag_entries, 0);
    assert!(registry.witness_readiness(system_now_secs()).is_ok());

    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("the supervisor should stop promptly")
        .expect("the supervisor task should not panic")
        .expect("the supervisor should shut down cleanly");
}

#[cfg(unix)]
#[tokio::test]
async fn registry_serves_only_the_durable_quorum_head_and_survives_restart() {
    let (state, client) = start_witness().await;
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(directory.path()).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(directory.path(), permissions).unwrap();
    }
    let database = directory.path().join("registry.db");
    let policy = WitnessPolicy::new(vec![client.witness_key()], 1, 120, 10, 0).unwrap();
    let registry = Arc::new(
        Registry::open(
            database.to_str().unwrap(),
            RegistryConfig {
                origin: ORIGIN.into(),
                signing_key: state.operator.clone(),
                allow_mock_identities: true,
            },
        )
        .unwrap()
        .with_witness_policy(policy.clone())
        .unwrap(),
    );
    let supervisor = WitnessSupervisor::new(
        Arc::clone(&registry),
        vec![client.clone()],
        Duration::from_secs(1),
        Duration::from_millis(10),
        Duration::from_millis(20),
    )
    .unwrap();

    assert!(matches!(
        registry.witness_readiness(NOW),
        Err(RegistryError::WitnessUnavailable)
    ));

    // Persist a witnessed empty checkpoint before the first append.  The next
    // sync must advance from 0 to 1 without asking storage for a consistency
    // proof, because the empty-tree transition has an empty proof by definition.
    let empty_report = supervisor.sync_once(NOW).await.unwrap();
    assert_eq!(empty_report.succeeded, 1);
    assert!(empty_report.promoted);
    assert_eq!(empty_report.publication.committed_size, 0);
    assert_eq!(empty_report.publication.published_size, 0);
    assert_eq!(
        registry
            .witness_receipt(WITNESS_NAME)
            .unwrap()
            .unwrap()
            .size(),
        0
    );

    let mutation = signed_directory_add(&SigningKey::from_bytes(&[44; 32]), 1);
    let pending = registry.append_directory_add(mutation.clone()).unwrap();
    assert!(pending.appended);
    assert_eq!(pending.index, 0);
    assert_eq!(pending.inclusion.size, 0);
    assert!(pending.inclusion.path.is_empty());
    assert_eq!(registry.committed_size().unwrap(), 1);
    assert_eq!(registry.size().unwrap(), 0);
    assert!(registry.entries(0, 10).unwrap().is_empty());
    assert!(matches!(registry.entry(0), Err(RegistryError::NotFound)));

    // Exact retries are pollable, but cannot be mistaken for final inclusion before quorum.
    let retry = registry.append_directory_add(mutation.clone()).unwrap();
    assert!(!retry.appended);
    assert_eq!(retry.index, 0);
    assert!(retry.index >= retry.inclusion.size);
    assert!(retry.inclusion.path.is_empty());

    let report = supervisor.sync_once(NOW).await.unwrap();
    assert_eq!(report.attempted, 1);
    assert_eq!(report.succeeded, 1);
    assert!(report.promoted);
    assert_eq!(report.publication.committed_size, 1);
    assert_eq!(report.publication.published_size, 1);
    assert_eq!(report.publication.lag_entries, 0);
    assert_eq!(registry.entries(0, 10).unwrap().len(), 1);
    assert_eq!(registry.entry(0).unwrap().seq(), 0);
    assert!(registry.witness_readiness(NOW).is_ok());

    let final_retry = registry.append_directory_add(mutation).unwrap();
    assert!(!final_retry.appended);
    assert_eq!(final_retry.inclusion.size, 1);
    assert!(final_retry.index < final_retry.inclusion.size);
    Checkpoint::verify_with_fresh_witnesses(
        &final_retry.inclusion.checkpoint,
        &state.operator.verifying_key(),
        &[client.witness_key()],
        1,
        NOW,
        120,
        10,
    )
    .unwrap();

    drop(supervisor);
    drop(registry);
    let reopened = Registry::open(
        database.to_str().unwrap(),
        RegistryConfig {
            origin: ORIGIN.into(),
            signing_key: state.operator.clone(),
            allow_mock_identities: true,
        },
    )
    .unwrap()
    .with_witness_policy(policy)
    .unwrap();
    assert_eq!(reopened.size().unwrap(), 1);
    assert_eq!(reopened.committed_size().unwrap(), 1);
    assert!(reopened.witness_readiness(NOW).is_ok());
    assert_eq!(reopened.entries(0, 10).unwrap().len(), 1);

    // Storage validates every receipt against local tree history, and policy installation also
    // revalidates the independent signature with the configured witness key.
    drop(reopened);
    let connection = Connection::open(&database).unwrap();
    let json: String = connection
        .query_row(
            "SELECT receipt_json FROM witness_receipts WHERE witness_name = ?1",
            [WITNESS_NAME],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let note = value["note"].as_str().unwrap();
    let mut changed = note.to_owned();
    let position = changed.len() - 3;
    let replacement = if &changed[position..position + 1] == "A" {
        "B"
    } else {
        "A"
    };
    changed.replace_range(position..position + 1, replacement);
    value["note"] = serde_json::Value::String(changed);
    connection
        .execute(
            "UPDATE witness_receipts SET receipt_json = ?1 WHERE witness_name = ?2",
            rusqlite::params![serde_json::to_string(&value).unwrap(), WITNESS_NAME],
        )
        .unwrap();
    drop(connection);
    let reopened = Registry::open(
        database.to_str().unwrap(),
        RegistryConfig {
            origin: ORIGIN.into(),
            signing_key: state.operator.clone(),
            allow_mock_identities: true,
        },
    )
    .unwrap();
    assert!(matches!(
        reopened.with_witness_policy(
            WitnessPolicy::new(vec![client.witness_key()], 1, 120, 10, 0,).unwrap()
        ),
        Err(RegistryError::WitnessConflict)
    ));
}

#[cfg(unix)]
fn signed_directory_add(loft: &SigningKey, mutation_sequence: u64) -> DirectoryAdd {
    let endpoint = "https://loft.test";
    let loft_pubkey = lower_hex(loft.verifying_key().as_bytes());
    let payload = directory_add_claim_payload(
        endpoint,
        &loft_pubkey,
        None,
        1,
        30,
        true,
        0,
        1024 * 1024,
        mutation_sequence,
    )
    .unwrap();
    let signature = lower_hex(&loft.sign(&payload).to_bytes());
    DirectoryAdd::authenticated(
        endpoint.into(),
        loft_pubkey,
        None,
        1,
        30,
        true,
        0,
        1024 * 1024,
        mutation_sequence,
        signature,
    )
    .unwrap()
}

#[cfg(unix)]
fn lower_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(unix)]
fn system_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must be after the Unix epoch")
        .as_secs()
}

fn decode_hash(input: &str) -> Option<Hash> {
    let bytes = decode_base64(input)?;
    bytes.try_into().ok()
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if input.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::new();
    for chunk in input.as_bytes().chunks_exact(4) {
        let mut values = [0u8; 4];
        let mut padding = 0;
        for (index, byte) in chunk.iter().copied().enumerate() {
            if byte == b'=' {
                values[index] = 0;
                padding += 1;
            } else {
                values[index] =
                    u8::try_from(ALPHABET.iter().position(|item| *item == byte)?).ok()?;
            }
        }
        let value = (u32::from(values[0]) << 18)
            | (u32::from(values[1]) << 12)
            | (u32::from(values[2]) << 6)
            | u32::from(values[3]);
        out.push((value >> 16) as u8);
        if padding < 2 {
            out.push((value >> 8) as u8);
        }
        if padding == 0 {
            out.push(value as u8);
        }
    }
    Some(out)
}
