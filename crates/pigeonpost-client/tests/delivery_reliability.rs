//! Adversarial delivery tests for terminal failures and bounded foreground wake-ups.

use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use pigeonpost_client::agent::now;
use pigeonpost_client::state::OutboxRoute;
use pigeonpost_client::{Agent, ClientError, StorageLimits, StorageResource, WakeupLimits};
use pigeonpost_core::{envelope, keys::SuccessorCommitment, AgentRecord, Identity};
use pigeonpost_loft::wire::AgentRecordRequest;
use serde_json::{json, Value};

#[derive(Clone)]
struct Probe {
    status: Arc<AtomicU16>,
    delay_ms: u64,
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl Probe {
    fn new(status: StatusCode, delay: Duration) -> Self {
        Self {
            status: Arc::new(AtomicU16::new(status.as_u16())),
            delay_ms: delay.as_millis() as u64,
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn enter(&self) {
        self.started.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.completed.fetch_add(1, Ordering::SeqCst);
    }
}

struct TestServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn publish(State(probe): State<Probe>) -> (StatusCode, Json<Value>) {
    probe.enter().await;
    let status = StatusCode::from_u16(probe.status.load(Ordering::SeqCst)).unwrap();
    let body = if status.is_success() {
        json!({ "id": "accepted", "stored": true })
    } else {
        json!({ "error": "sensitive server detail must never enter durable state" })
    };
    (status, Json(body))
}

async fn fetch(State(probe): State<Probe>) -> (StatusCode, Json<Value>) {
    probe.enter().await;
    (
        StatusCode::OK,
        Json(json!({ "events": [], "next_cursor": 0, "more": false })),
    )
}

async fn spawn_probe(probe: Probe) -> TestServer {
    let app = Router::new()
        .route("/v1/publish", post(publish))
        .route("/v1/fetch", post(fetch))
        .with_state(probe);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer {
        url: format!("http://{address}"),
        task,
    }
}

async fn overflow_cursor(State(event): State<pigeonpost_core::envelope::Wrap>) -> Json<Value> {
    Json(json!({
        "events": [event],
        "next_cursor": u64::MAX,
        "more": false
    }))
}

async fn spawn_overflow_cursor(event: pigeonpost_core::envelope::Wrap) -> TestServer {
    let app = Router::new()
        .route("/v1/fetch", post(overflow_cursor))
        .with_state(event);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer {
        url: format!("http://{address}"),
        task,
    }
}

async fn quota_events(
    State(events): State<Arc<Vec<pigeonpost_core::envelope::Wrap>>>,
) -> Json<Value> {
    Json(json!({
        "events": events.as_ref(),
        "next_cursor": 2,
        "more": false
    }))
}

async fn spawn_quota_events(events: Vec<pigeonpost_core::envelope::Wrap>) -> TestServer {
    let app = Router::new()
        .route("/v1/fetch", post(quota_events))
        .with_state(Arc::new(events));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer {
        url: format!("http://{address}"),
        task,
    }
}

async fn hostile_agent_record(State(record): State<AgentRecord>) -> Json<AgentRecordRequest> {
    Json(AgentRecordRequest { record })
}

async fn spawn_overflow_record() -> (TestServer, pigeonpost_core::Address) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}");
    let target = Identity::from_seed([0x61; 32]);
    let successor = Identity::from_seed([0x62; 32]);
    let record = AgentRecord::new(
        &target,
        &SuccessorCommitment::for_key(&successor.verifying_key()),
        u64::MAX,
        vec![url.clone()],
    );
    let target_address = target.address();
    let app = Router::new()
        .route("/v1/agent/{address}", get(hostile_agent_record))
        .route(
            "/v1/rotation/{address}",
            get(|| async { StatusCode::NOT_FOUND }),
        )
        .with_state(record);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (TestServer { url, task }, target_address)
}

fn sample_wrap() -> pigeonpost_core::envelope::Wrap {
    let sender = Identity::from_seed([0x41; 32]);
    let recipient = Identity::from_seed([0x42; 32]);
    envelope::wrap(&sender, &recipient.verifying_key(), "bounded wakeup", now()).unwrap()
}

fn queue(agent: &Agent, message_id: &str, url: &str) {
    agent
        .state()
        .queue(
            message_id,
            "/k/test",
            OutboxRoute::new(url, true),
            &sample_wrap(),
            None,
            now(),
        )
        .unwrap();
}

#[tokio::test]
async fn permanent_http_failure_is_durable_terminal_state_without_body_reflection() {
    let probe = Probe::new(StatusCode::BAD_REQUEST, Duration::ZERO);
    let server = spawn_probe(probe).await;
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    queue(&agent, "permanent", &server.url);

    let report = agent
        .flush_with_limits(WakeupLimits::new(2, Duration::from_secs(1)).unwrap())
        .await
        .unwrap();
    assert_eq!(report.attempted, 1);
    assert_eq!(report.terminal, 1);
    assert_eq!(report.queued, 0);
    assert_eq!(report.dead_letters, 1);
    let letters = agent.dead_letters(10).unwrap();
    assert_eq!(letters[0].reason, "http_400");
    assert!(!letters[0].reason.contains("sensitive"));

    let again = agent.flush().await.unwrap();
    assert_eq!(again.attempted, 0, "terminal rows are not retried");
    drop(agent);
    let reopened = Agent::open(&home).unwrap();
    assert_eq!(reopened.state().terminal_count().unwrap(), 1);
    assert_eq!(reopened.state().pending_count().unwrap(), 0);
}

#[tokio::test]
async fn retryable_http_failure_recovers_on_a_later_wakeup() {
    let probe = Probe::new(StatusCode::SERVICE_UNAVAILABLE, Duration::ZERO);
    let server = spawn_probe(probe.clone()).await;
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    queue(&agent, "transient", &server.url);

    let failed = agent.flush().await.unwrap();
    assert_eq!(failed.retryable, 1);
    assert_eq!(failed.terminal, 0);
    assert_eq!(failed.queued, 1);
    probe
        .status
        .store(StatusCode::OK.as_u16(), Ordering::SeqCst);

    // The first retry delay is five seconds plus at most one second of deterministic jitter.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let recovered = agent.flush().await.unwrap();
    assert_eq!(recovered.delivered, 1);
    assert_eq!(recovered.queued, 0);
    assert_eq!(recovered.dead_letters, 0);
}

#[tokio::test]
async fn flush_parallelism_is_bounded_and_deadline_cancellation_cannot_mutate_later() {
    let concurrent_probe = Probe::new(StatusCode::OK, Duration::from_millis(150));
    let mut servers = Vec::new();
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    for index in 0..6 {
        let server = spawn_probe(concurrent_probe.clone()).await;
        queue(&agent, &format!("parallel-{index}"), &server.url);
        servers.push(server);
    }

    let report = agent
        .flush_with_limits(WakeupLimits::new(3, Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    assert_eq!(report.attempted, 6);
    assert_eq!(report.delivered, 6);
    assert!(!report.deadline_exceeded);
    assert_eq!(concurrent_probe.started.load(Ordering::SeqCst), 6);
    assert_eq!(concurrent_probe.completed.load(Ordering::SeqCst), 6);
    assert_eq!(concurrent_probe.max_active.load(Ordering::SeqCst), 3);

    let deadline_probe = Probe::new(StatusCode::OK, Duration::from_millis(250));
    let deadline_home_root = tempfile::tempdir().unwrap();
    let deadline_home = deadline_home_root.path().join("agent");
    let deadline_agent = Agent::open(&deadline_home).unwrap();
    let mut deadline_servers = Vec::new();
    for index in 0..6 {
        let server = spawn_probe(deadline_probe.clone()).await;
        queue(&deadline_agent, &format!("deadline-{index}"), &server.url);
        deadline_servers.push(server);
    }

    let report = deadline_agent
        .flush_with_limits(WakeupLimits::new(2, Duration::from_millis(50)).unwrap())
        .await
        .unwrap();
    assert!(report.deadline_exceeded);
    assert_eq!(report.cancelled, 2);
    assert_eq!(report.attempted, 0);
    assert_eq!(report.queued, 6);
    assert!(deadline_probe.started.load(Ordering::SeqCst) <= 2);
    let started = deadline_probe.started.load(Ordering::SeqCst);

    // The servers may finish requests the client cancelled, but no queued request may start and no
    // state mutation may happen after `flush_with_limits` has returned.
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(deadline_probe.started.load(Ordering::SeqCst), started);
    assert_eq!(deadline_agent.state().pending_count().unwrap(), 6);
    assert_eq!(deadline_agent.state().terminal_count().unwrap(), 0);
    drop(deadline_servers);
    drop(servers);
}

#[tokio::test]
async fn drain_parallelism_and_whole_wakeup_deadline_are_bounded() {
    let probe = Probe::new(StatusCode::OK, Duration::from_millis(250));
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    let mut servers = Vec::new();
    for _ in 0..6 {
        let server = spawn_probe(probe.clone()).await;
        agent
            .state()
            .add_loft_with_local_trust(&server.url, Some([0x77; 32]), now(), true)
            .unwrap();
        servers.push(server);
    }

    let report = agent
        .drain_with_limits(WakeupLimits::new(2, Duration::from_millis(50)).unwrap())
        .await
        .unwrap();
    assert!(report.deadline_exceeded);
    assert_eq!(report.lofts_failed.len(), 6);
    assert!(probe.started.load(Ordering::SeqCst) <= 2);
    assert!(probe.max_active.load(Ordering::SeqCst) <= 2);
    let started = probe.started.load(Ordering::SeqCst);

    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(probe.started.load(Ordering::SeqCst), started);
    for server in &servers {
        assert_eq!(
            agent.state().cursor(&server.url, &agent.address()).unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn unpersistable_loft_cursor_is_rejected_before_batch_processing() {
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    let sender = Identity::from_seed([0x51; 32]);
    let event = envelope::wrap(
        &sender,
        &agent.verifying_key(),
        "cursor overflow must not be stored",
        now(),
    )
    .unwrap();
    let server = spawn_overflow_cursor(event).await;
    agent
        .state()
        .add_loft_with_local_trust(&server.url, Some([0x71; 32]), now(), true)
        .unwrap();

    let report = agent.drain().await.unwrap();
    assert_eq!(report.fetched, 0);
    assert_eq!(report.new_messages, 0);
    assert_eq!(report.lofts_failed, vec![server.url.clone()]);
    assert_eq!(
        agent.state().cursor(&server.url, &agent.address()).unwrap(),
        0
    );
    assert!(agent.state().messages(false, 10).unwrap().is_empty());

    drop(agent);
    let reopened = Agent::open(&home).unwrap();
    assert_eq!(
        reopened
            .state()
            .cursor(&server.url, &reopened.address())
            .unwrap(),
        0
    );
    assert!(reopened.state().messages(false, 10).unwrap().is_empty());
}

#[tokio::test]
async fn inbox_quota_failure_keeps_the_live_route_cursor_for_idempotent_retry() {
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    agent.set_accept_all(true).unwrap();
    let sender = Identity::from_seed([0x52; 32]);
    let events = vec![
        envelope::wrap(
            &sender,
            &agent.verifying_key(),
            "first retained event",
            now(),
        )
        .unwrap(),
        envelope::wrap(
            &sender,
            &agent.verifying_key(),
            "second retained event",
            now() + 1,
        )
        .unwrap(),
    ];
    let server = spawn_quota_events(events).await;
    agent
        .state()
        .add_loft_with_local_trust(&server.url, Some([0x73; 32]), now(), true)
        .unwrap();
    agent
        .set_storage_limits(StorageLimits {
            inbox_messages: 1,
            ..StorageLimits::default()
        })
        .unwrap();

    assert!(matches!(
        agent.drain().await,
        Err(ClientError::StorageLimit(StorageResource::InboxMessages))
    ));
    assert_eq!(
        agent.state().cursor(&server.url, &agent.address()).unwrap(),
        0
    );
    assert_eq!(agent.storage_status().unwrap().usage.inbox_messages, 1);

    let current = agent.storage_status().unwrap();
    agent
        .set_storage_limits(StorageLimits {
            inbox_messages: 2,
            ..current.limits
        })
        .unwrap();
    let retried = agent.drain().await.unwrap();
    assert_eq!(retried.duplicates, 1);
    assert_eq!(retried.new_messages, 1);
    assert_eq!(
        agent.state().cursor(&server.url, &agent.address()).unwrap(),
        2
    );
    assert_eq!(agent.storage_status().unwrap().usage.inbox_messages, 2);
}

#[tokio::test]
async fn signed_unpersistable_agent_record_cannot_poison_resolution_state() {
    let (server, target) = spawn_overflow_record().await;
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    agent
        .state()
        .add_loft_with_local_trust(&server.url, Some([0x72; 32]), now(), true)
        .unwrap();

    assert!(agent.resolve(&target).await.is_err());
    assert!(agent.state().resolution(&target).unwrap().is_none());

    drop(agent);
    let reopened = Agent::open(&home).unwrap();
    assert!(reopened.state().resolution(&target).unwrap().is_none());
}
