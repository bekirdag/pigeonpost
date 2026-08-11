//! In-crate M4 acceptance: a second loft joins by submission, is probed, and is then selected by a client
//! through capacity weighting.
//!
//! Real lofts on real sockets, probed by the real prober. The point is not that the code runs —
//! it is that the mechanism `docs/capacity.md` depends on actually moves traffic.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use pigeonpost_core::{envelope::Wrap, Identity};
use pigeonpost_directory::{
    directory::PROMOTE_AFTER_SECS,
    prober::{probe_once_for_test, sweep_for_test},
    Directory, DirectoryEntry, LoftPolicy, LoftState, Rng, SelectionCriteria,
};
use pigeonpost_loft::wire::{
    FetchRequest, FetchResponse, InfoResponse, PublishRequest, PublishResponse,
};
use pigeonpost_loft::{Loft, LoftConfig, LoftStore, SqliteStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DishonestMode {
    AckAndDrop,
    RetentionLiar,
}

struct DishonestLoft {
    mode: DishonestMode,
    origin: String,
    pubkey: [u8; 32],
    events: Mutex<Vec<Wrap>>,
}

async fn dishonest_info(State(state): State<Arc<DishonestLoft>>) -> Json<InfoResponse> {
    if state.mode == DishonestMode::RetentionLiar {
        // A retention liar can serve fresh writes while discarding everything between probes.
        state.events.lock().unwrap().clear();
    }
    Json(InfoResponse {
        software: "dishonest-loft-fixture".into(),
        version: "0.2.0".into(),
        protocol: pigeonpost_core::PROTOCOL_VERSION.into(),
        pubkey: pigeonpost_directory::entry::hex(&state.pubkey),
        origin: state.origin.clone(),
        capacity_bytes: 100 * 1024 * 1024 * 1024,
        used_bytes: 0,
        utilization: 0.0,
        retention_days: 30,
        open: true,
        pow_floor: 0,
        max_event_bytes: pigeonpost_loft::wire::MAX_EVENT_BYTES,
        event_count: state.events.lock().unwrap().len() as u64,
        accepting: true,
    })
}

async fn dishonest_publish(
    State(state): State<Arc<DishonestLoft>>,
    Json(request): Json<PublishRequest>,
) -> Json<PublishResponse> {
    let id = request.wrap.id();
    if state.mode == DishonestMode::RetentionLiar {
        state.events.lock().unwrap().push(request.wrap);
    }
    Json(PublishResponse {
        id: pigeonpost_directory::entry::hex(&id),
        stored: true,
    })
}

async fn dishonest_fetch(
    State(state): State<Arc<DishonestLoft>>,
    Json(request): Json<FetchRequest>,
) -> Json<FetchResponse> {
    let events: Vec<_> = state
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.recipient == request.auth.recipient)
        .cloned()
        .collect();
    Json(FetchResponse {
        next_cursor: request.auth.cursor.saturating_add(events.len() as u64),
        events,
        more: false,
    })
}

async fn spawn_dishonest_loft(mode: DishonestMode, seed: u8) -> (String, SigningKey) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(DishonestLoft {
        mode,
        origin: origin.clone(),
        pubkey: key.verifying_key().to_bytes(),
        events: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/v1/info", get(dishonest_info))
        .route("/v1/publish", post(dishonest_publish))
        .route("/v1/fetch", post(dishonest_fetch))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await });
    (origin, key)
}

/// Boot a loft and return its URL plus the key it signs directory entries with.
async fn spawn_loft(seed: u8, capacity_gb: u64) -> (String, SigningKey) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let pubkey = key.verifying_key().to_bytes();
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let config = LoftConfig::new(pubkey, &url)
        .with_capacity_bytes(capacity_gb * 1024 * 1024 * 1024)
        .with_rate_limit(10_000);
    let loft = Arc::new(Loft::new(config, store).unwrap());

    tokio::spawn(
        async move { pigeonpost_loft::serve(listener, loft, std::future::pending()).await },
    );
    (url, key)
}

fn entry_for(key: &SigningKey, endpoint: &str, capacity_gb: u64, operator: &str) -> DirectoryEntry {
    DirectoryEntry::signed(
        key,
        endpoint,
        Some(operator.to_string()),
        capacity_gb,
        30,
        LoftPolicy {
            open: true,
            pow_floor: 0,
            max_event_bytes: pigeonpost_loft::wire::MAX_EVENT_BYTES,
        },
        0.0,
    )
}

async fn probe_test(
    identity: &Identity,
    endpoint: &str,
    key: &SigningKey,
    now: u64,
) -> pigeonpost_directory::ProbeResult {
    let entry = entry_for(key, endpoint, 100, "/test/prober");
    probe_once_for_test(identity, &entry, now).await
}

#[tokio::test]
async fn a_loft_joins_is_probed_and_becomes_selectable() {
    let directory = Directory::in_memory().unwrap();
    let prober = Identity::from_seed([0xAA; 32]);

    let (url, key) = spawn_loft(1, 100).await;

    // 1. It submits itself. Open admission: no approval step.
    directory
        .submit(entry_for(&key, &url, 100, "/github/first"), 0)
        .unwrap();
    assert_eq!(
        directory.entry(&url).unwrap().state,
        LoftState::Pending,
        "a fresh submission is not selectable yet"
    );

    // 2. The prober actually talks to it: writes a test event and reads it back.
    let result = probe_test(&prober, &url, &key, 100).await;
    assert!(result.reachable, "detail: {:?}", result.detail);
    assert!(
        result.stored_and_returned,
        "a loft that accepts writes and drops them must be caught: {:?}",
        result.detail
    );

    // 3. After probing clean past the promotion window, it goes active.
    directory.record_probe(&result, 100).unwrap();
    let promoted = probe_test(&prober, &url, &key, 100 + PROMOTE_AFTER_SECS).await;
    let state = directory
        .record_probe(&promoted, 100 + PROMOTE_AFTER_SECS)
        .unwrap();
    assert_eq!(state, LoftState::Active);

    // 4. And now a client will pick it.
    let pool = directory.entries().unwrap();
    let chosen = pigeonpost_directory::select(
        None,
        &[],
        &pool,
        &SelectionCriteria::default(),
        &mut Rng::seeded(1),
    );
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].endpoint, url);
}

#[tokio::test]
async fn an_acknowledge_and_drop_loft_is_detected_and_deweighted() {
    let directory = Arc::new(Directory::in_memory().unwrap());
    let prober = Identity::from_seed([0xA1; 32]);
    let (endpoint, key) = spawn_dishonest_loft(DishonestMode::AckAndDrop, 0xA2).await;
    directory
        .submit(entry_for(&key, &endpoint, 100, "/github/ackdrop"), 0)
        .unwrap();
    directory.set_state(&endpoint, LoftState::Active).unwrap();

    for now in [1_000, 1_300, 1_600] {
        assert_eq!(
            sweep_for_test(Arc::clone(&directory), &prober, now)
                .await
                .unwrap(),
            1
        );
        let latest = directory.probes(&endpoint, 1).unwrap().pop().unwrap();
        assert!(latest.reachable, "detail: {:?}", latest.detail);
        assert!(!latest.stored_and_returned);
        assert!(latest
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("expected probe event")));
    }
    assert_eq!(
        directory.entry(&endpoint).unwrap().state,
        LoftState::Degraded
    );
    assert!(pigeonpost_directory::select(
        None,
        &[],
        &directory.entries().unwrap(),
        &SelectionCriteria::default(),
        &mut Rng::seeded(0xA3),
    )
    .is_empty());
}

#[tokio::test]
async fn a_retention_liar_is_caught_by_the_aged_canary_and_deweighted() {
    let directory = Arc::new(Directory::in_memory().unwrap());
    let prober = Identity::from_seed([0xB1; 32]);
    let (endpoint, key) = spawn_dishonest_loft(DishonestMode::RetentionLiar, 0xB2).await;
    directory
        .submit(entry_for(&key, &endpoint, 100, "/github/retention-liar"), 0)
        .unwrap();

    let created_at = 10_000;
    assert_eq!(
        sweep_for_test(Arc::clone(&directory), &prober, created_at)
            .await
            .unwrap(),
        1
    );
    assert!(directory.probes(&endpoint, 1).unwrap()[0].healthy());
    directory.set_state(&endpoint, LoftState::Active).unwrap();

    for now in [
        created_at + 86_400,
        created_at + 86_700,
        created_at + 87_000,
    ] {
        assert_eq!(
            sweep_for_test(Arc::clone(&directory), &prober, now)
                .await
                .unwrap(),
            1
        );
        let latest = directory.probes(&endpoint, 1).unwrap().pop().unwrap();
        assert!(latest.reachable, "detail: {:?}", latest.detail);
        assert!(latest.stored_and_returned, "fresh liveness probe must pass");
        assert_eq!(latest.retention_ok, Some(false));
        assert!(latest
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("retention canary check failed")));
    }
    assert_eq!(
        directory.entry(&endpoint).unwrap().state,
        LoftState::Degraded
    );
    assert!(pigeonpost_directory::select(
        None,
        &[],
        &directory.entries().unwrap(),
        &SelectionCriteria::default(),
        &mut Rng::seeded(0xB3),
    )
    .is_empty());
}

#[tokio::test]
async fn a_second_loft_takes_a_share_proportional_to_its_capacity() {
    // This is the whole mechanism: as capacity joins the pool, our share of new agents falls
    // without anyone migrating anything.
    let directory = Arc::new(Directory::in_memory().unwrap());
    let prober = Identity::from_seed([0xAA; 32]);

    let (ours, our_key) = spawn_loft(1, 100).await;
    let (theirs, their_key) = spawn_loft(2, 900).await;

    for (url, key, operator) in [
        (&ours, &our_key, "/github/us"),
        (&theirs, &their_key, "/github/them"),
    ] {
        let capacity = if operator == "/github/us" { 100 } else { 900 };
        directory
            .submit(entry_for(key, url, capacity, operator), 0)
            .unwrap();
    }

    // Probe both into service.
    sweep_for_test(Arc::clone(&directory), &prober, 100)
        .await
        .unwrap();
    sweep_for_test(Arc::clone(&directory), &prober, 100 + PROMOTE_AFTER_SECS)
        .await
        .unwrap();

    let pool = directory.entries().unwrap();
    assert!(pool.iter().all(|e| e.state == LoftState::Active));

    let mut ours_picked = 0;
    for seed in 1..1001u64 {
        let chosen = pigeonpost_directory::select(
            None,
            &[],
            &pool,
            &SelectionCriteria {
                target: 1,
                ..Default::default()
            },
            &mut Rng::seeded(seed),
        );
        if chosen[0].endpoint == ours {
            ours_picked += 1;
        }
    }

    let share = ours_picked as f64 / 1000.0;
    assert!(
        (0.05..0.16).contains(&share),
        "our 10% of pool capacity should draw ~10% of new agents, got {share:.3}"
    );
}

#[tokio::test]
async fn a_loft_that_goes_away_is_de_weighted() {
    let directory = Directory::in_memory().unwrap();
    let prober = Identity::from_seed([0xAA; 32]);

    let (url, key) = spawn_loft(1, 100).await;
    directory
        .submit(entry_for(&key, &url, 100, "/github/first"), 0)
        .unwrap();
    directory.set_state(&url, LoftState::Active).unwrap();

    // A URL nothing is listening on stands in for a node that vanished.
    let dead = "http://127.0.0.1:1";
    let (_, dead_key) = spawn_loft(3, 100).await;
    directory
        .submit(entry_for(&dead_key, dead, 100, "/github/dead"), 0)
        .unwrap();
    directory.set_state(dead, LoftState::Active).unwrap();

    let mut now = 1_000;
    let mut dead_state = LoftState::Active;
    for _ in 0..4 {
        now += 300;
        let result = probe_test(&prober, dead, &dead_key, now).await;
        assert!(!result.reachable);
        dead_state = directory.record_probe(&result, now).unwrap();
    }
    assert_eq!(dead_state, LoftState::Degraded);

    // A client now picks only the live one.
    let pool = directory.entries().unwrap();
    let chosen = pigeonpost_directory::select(
        None,
        &[],
        &pool,
        &SelectionCriteria::default(),
        &mut Rng::seeded(9),
    );
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].endpoint, url);
}

#[tokio::test]
async fn a_full_loft_is_measured_as_unhealthy_rather_than_trusted() {
    // Over-advertising is self-correcting: claim capacity you do not have, start refusing writes,
    // lose weight. Nobody has to police it.
    let directory = Directory::in_memory().unwrap();
    let prober = Identity::from_seed([0xAA; 32]);

    let key = SigningKey::from_bytes(&[7; 32]);
    let pubkey = key.verifying_key().to_bytes();
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    // Two kilobytes of capacity, advertised as a hundred gigabytes.
    let loft = Arc::new(
        Loft::new(
            LoftConfig::new(pubkey, &url).with_capacity_bytes(2_000),
            store,
        )
        .unwrap(),
    );

    tokio::spawn(
        async move { pigeonpost_loft::serve(listener, loft, std::future::pending()).await },
    );

    directory
        .submit(entry_for(&key, &url, 100, "/github/liar"), 0)
        .unwrap();
    directory.set_state(&url, LoftState::Active).unwrap();

    // Fill it, then probe.
    let filler = Identity::from_seed([0xBB; 32]);
    let client = pigeonpost_loft::LoftClient::new(&url).unwrap();
    for i in 0..10 {
        let wrap = pigeonpost_core::envelope::wrap(
            &filler,
            &filler.verifying_key(),
            &format!("filler {i}"),
            1_000,
        )
        .unwrap();
        let _ = client.publish(&wrap, None).await;
    }

    let result = probe_test(&prober, &url, &key, 2_000).await;
    assert!(result.reachable, "it is up — it is just full");
    assert_eq!(
        result.utilization, 1.0,
        "a mismatched live claim must be persisted at the worst safe weight"
    );
    assert!(
        !result.stored_and_returned,
        "a loft that cannot take mail must not keep its weight"
    );

    let mut now = 2_000;
    let mut state = LoftState::Active;
    for _ in 0..3 {
        now += 300;
        state = directory
            .record_probe(&probe_test(&prober, &url, &key, now).await, now)
            .unwrap();
    }
    assert_eq!(state, LoftState::Degraded);
}

#[tokio::test]
async fn rendezvous_lets_a_sender_find_a_record_it_has_no_prior_knowledge_of() {
    // Closes the bootstrap loop: the agent record lists the lofts, but you need a loft to fetch
    // the record. Both sides compute the same three lofts from the address alone.
    let directory = Arc::new(Directory::in_memory().unwrap());
    let prober = Identity::from_seed([0xAA; 32]);

    let mut urls = Vec::new();
    for seed in 1..=5u8 {
        let (url, key) = spawn_loft(seed, 100).await;
        directory
            .submit(entry_for(&key, &url, 100, &format!("/github/op{seed}")), 0)
            .unwrap();
        urls.push(url);
    }
    sweep_for_test(Arc::clone(&directory), &prober, 100)
        .await
        .unwrap();
    sweep_for_test(Arc::clone(&directory), &prober, 100 + PROMOTE_AFTER_SECS)
        .await
        .unwrap();

    let pool = directory.entries().unwrap();
    // The in-process transports necessarily share loopback, while real rendezvous candidates
    // must occupy independent authenticated host failure domains. Present distinct simulated
    // hosts for selection, then map the selected verified keys back to their live test sockets.
    let routing_pool: Vec<_> = pool
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, mut entry)| {
            entry.endpoint = format!("https://loft-{index}.fixture.invalid");
            entry
        })
        .collect();
    let agent = Identity::from_seed([0xCC; 32]);
    let address = agent.address();

    let publisher_targets = pigeonpost_directory::rendezvous(&routing_pool, address.as_str(), 3);
    let sender_targets = pigeonpost_directory::rendezvous(&routing_pool, address.as_str(), 3);

    assert_eq!(publisher_targets.len(), 3);
    assert_eq!(
        publisher_targets
            .iter()
            .map(|entry| &entry.pubkey)
            .collect::<Vec<_>>(),
        sender_targets
            .iter()
            .map(|entry| &entry.pubkey)
            .collect::<Vec<_>>()
    );

    // And the record really is retrievable from those three and nowhere else in particular.
    use pigeonpost_core::{keys::SuccessorCommitment, AgentRecord};
    let successor = SuccessorCommitment::for_key(&Identity::from_seed([0xDD; 32]).verifying_key());
    let record = AgentRecord::new(&agent, &successor, 1, vec![]);

    for target in &publisher_targets {
        let live_endpoint = &pool
            .iter()
            .find(|entry| entry.pubkey == target.pubkey)
            .unwrap()
            .endpoint;
        pigeonpost_loft::LoftClient::new(live_endpoint)
            .unwrap()
            .put_agent_record(&address, &record)
            .await
            .unwrap();
    }

    let sender_endpoint = &pool
        .iter()
        .find(|entry| entry.pubkey == sender_targets[0].pubkey)
        .unwrap()
        .endpoint;
    let found = pigeonpost_loft::LoftClient::new(sender_endpoint)
        .unwrap()
        .agent_record(&address)
        .await
        .unwrap();
    assert_eq!(found.pubkey, agent.verifying_key().to_bytes());
}

#[tokio::test]
async fn the_http_surface_serves_a_usable_directory() {
    let directory = Arc::new(Directory::in_memory().unwrap());
    let (url, key) = spawn_loft(1, 100).await;
    directory
        .submit(entry_for(&key, &url, 100, "/github/first"), 0)
        .unwrap();
    directory.set_state(&url, LoftState::Active).unwrap();
    directory
        .mark_probe_sweep(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = pigeonpost_directory::server::router(Arc::clone(&directory));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });

    let signing_key = directory.signing_public_key();
    let document: pigeonpost_directory::DirectoryDocument =
        reqwest::get(format!("{base}/directory.json"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

    document.verify(&signing_key).unwrap();
    assert_eq!(document.version, 1);
    assert_eq!(document.lofts.len(), 1);
    assert_eq!(document.lofts[0].endpoint, url);

    let measurements: pigeonpost_directory::server::ProbeDocument = reqwest::Client::new()
        .get(format!("{base}/v1/probe"))
        .query(&[("endpoint", &url)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    measurements.verify(&signing_key).unwrap();
    assert_eq!(measurements.endpoint, url);

    // A submission over HTTP with a broken signature is refused.
    let mut forged = entry_for(&key, "https://forged.example", 100, "/github/x");
    forged.capacity_gb = 999;
    let response = reqwest::Client::new()
        .post(format!("{base}/v1/directory/submit"))
        .json(&serde_json::json!({ "entry": forged }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn probe_is_not_hidden_by_more_than_five_hundred_retained_probe_events() {
    // Regression: the old prober fetched only cursor=0/limit=500. Once its recipient retained 500
    // events, every healthy loft looked as though it accepted and dropped the new probe.
    let prober = Identity::from_seed([0xA5; 32]);
    let (url, key) = spawn_loft(1, 100).await;
    let client = pigeonpost_loft::LoftClient::new(&url).unwrap();
    for sequence in 0..501 {
        let wrap = pigeonpost_core::envelope::wrap(
            &prober,
            &prober.verifying_key(),
            &format!("retained probe history {sequence}"),
            1_000,
        )
        .unwrap();
        client.publish(&wrap, None).await.unwrap();
    }

    let result = probe_test(&prober, &url, &key, 2_000).await;
    assert!(result.reachable, "detail: {:?}", result.detail);
    assert!(
        result.stored_and_returned,
        "retained history must not hide the one-shot probe recipient: {:?}",
        result.detail
    );
}

#[tokio::test]
async fn probe_refuses_a_loft_whose_info_key_differs_from_the_submission() {
    let prober = Identity::from_seed([0xA5; 32]);
    let (url, actual_key) = spawn_loft(1, 100).await;
    let claimed_key = SigningKey::from_bytes(&[2; 32]);
    let before = pigeonpost_loft::LoftClient::new(&url)
        .unwrap()
        .info()
        .await
        .unwrap()
        .event_count;

    let result = probe_test(&prober, &url, &claimed_key, 2_000).await;
    assert!(result.reachable);
    assert!(!result.stored_and_returned);
    assert!(result
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("does not match")));
    let after = pigeonpost_loft::LoftClient::new(&url)
        .unwrap()
        .info()
        .await
        .unwrap()
        .event_count;
    assert_eq!(
        before, after,
        "key mismatch must stop before the prober writes anything"
    );
    assert_ne!(actual_key.verifying_key(), claimed_key.verifying_key());
}

#[tokio::test]
async fn probe_does_not_follow_redirects() {
    use axum::{
        http::{header, StatusCode},
        response::IntoResponse,
        routing::get,
        Router,
    };

    let (target, key) = spawn_loft(1, 100).await;
    let redirect = target.clone();
    let app = Router::new().route(
        "/v1/info",
        get(move || {
            let redirect = redirect.clone();
            async move {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(header::LOCATION, format!("{redirect}/v1/info"))],
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });

    let result = probe_test(&Identity::from_seed([0xA5; 32]), &endpoint, &key, 2_000).await;
    assert!(!result.reachable);
    assert!(result
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("307")));
}

#[tokio::test]
async fn probe_rejects_an_oversized_info_body_before_parsing_it() {
    use axum::{body::Body, http::Response, routing::get, Router};

    let app = Router::new().route(
        "/v1/info",
        get(|| async { Response::new(Body::from(vec![b'x'; 64 * 1024 + 1])) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let key = SigningKey::from_bytes(&[1; 32]);

    let result = probe_test(&Identity::from_seed([0xA5; 32]), &endpoint, &key, 2_000).await;
    assert!(!result.reachable);
    assert!(result
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("body limit")));
}
