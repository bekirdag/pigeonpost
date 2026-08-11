//! Product-path tests for witnessed attribution key discovery and offline cache use.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use pigeonpost_client::{Agent, AttributionRequirement, ClientError, OutboxRoute, WakeupLimits};
use pigeonpost_compliance_format::{
    attribution_epoch_end_ms, ComplianceKeyId, CompliancePurpose, Jurisdiction,
    TRACE_EPOCH_DURATION_MS,
};
use pigeonpost_core::envelope::{self, Attribution};
use pigeonpost_core::{keys, Identity, RecipientPolicy};
use pigeonpost_loft::{
    AttributionKeyResolver, AttributionResolutionError, Loft, LoftClient, LoftConfig, LoftStore,
    ResolvedAttributionKey, SqliteStore,
};
use pigeonpost_registry::{
    Checkpoint, CheckpointPin, ComplianceKeyPublish, ComplianceKeyStatus, LogEntry, MerkleLog,
    RegistryTrust, WitnessKey,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::sync::RwLock;

struct FixedResolver {
    key_id: ComplianceKeyId,
    key: ResolvedAttributionKey,
}

impl AttributionKeyResolver for FixedResolver {
    fn resolve(
        &self,
        key_id: &ComplianceKeyId,
    ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError> {
        Ok((*key_id == self.key_id).then_some(self.key))
    }
}

async fn spawn_loft(resolver: Option<FixedResolver>) -> (String, Arc<SqliteStore>) {
    spawn_loft_with_resolver(
        resolver.map(|resolver| Arc::new(resolver) as Arc<dyn AttributionKeyResolver>),
    )
    .await
}

async fn spawn_loft_with_resolver(
    resolver: Option<Arc<dyn AttributionKeyResolver>>,
) -> (String, Arc<SqliteStore>) {
    let pubkey = Identity::from_seed([0x41; 32]).verifying_key().to_bytes();
    let concrete_store = Arc::new(SqliteStore::in_memory().unwrap());
    let store: Arc<dyn LoftStore> = concrete_store.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut loft = Loft::new(LoftConfig::new(pubkey, &url), store).unwrap();
    if let Some(resolver) = resolver {
        loft = loft.with_attribution_resolver(resolver);
    }
    let loft = Arc::new(loft);
    tokio::spawn(
        async move { pigeonpost_loft::serve(listener, loft, std::future::pending()).await },
    );
    (url, concrete_store)
}

struct TransitionResolver {
    key_id: ComplianceKeyId,
    public_key: [u8; 32],
    not_before_ms: u64,
    not_after_ms: u64,
    status: Arc<AtomicU8>,
}

struct AvailabilityResolver {
    key_id: ComplianceKeyId,
    key: ResolvedAttributionKey,
    available: Arc<AtomicBool>,
}

impl AvailabilityResolver {
    fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }
}

impl AttributionKeyResolver for AvailabilityResolver {
    fn readiness(&self, _now_ms: u64) -> std::result::Result<(), AttributionResolutionError> {
        self.available
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or(AttributionResolutionError::Unavailable)
    }

    fn resolve(
        &self,
        key_id: &ComplianceKeyId,
    ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(AttributionResolutionError::Unavailable);
        }
        Ok((*key_id == self.key_id).then_some(self.key))
    }
}

impl TransitionResolver {
    fn retire(&self) {
        self.status.store(2, Ordering::SeqCst);
    }
}

impl AttributionKeyResolver for TransitionResolver {
    fn resolve(
        &self,
        key_id: &ComplianceKeyId,
    ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError> {
        Ok((*key_id == self.key_id).then(|| ResolvedAttributionKey {
            public_key: self.public_key,
            not_before_ms: self.not_before_ms,
            not_after_ms: self.not_after_ms,
            status: match self.status.load(Ordering::SeqCst) {
                1 => ComplianceKeyStatus::Active,
                2 => ComplianceKeyStatus::Retired,
                _ => ComplianceKeyStatus::Revoked,
            },
        }))
    }
}

async fn spawn_registry(
    projection: Value,
    entries: Value,
) -> (String, tokio::sync::oneshot::Sender<()>) {
    let projection = Arc::new(projection);
    let entries = Arc::new(entries);
    let app = Router::new()
        .route(
            "/v1/compliance-keys",
            get({
                let projection = Arc::clone(&projection);
                move || {
                    let projection = Arc::clone(&projection);
                    async move { Json((*projection).clone()) }
                }
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
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (url, shutdown_tx)
}

struct ToggleRegistry {
    url: String,
    online: Arc<AtomicBool>,
    projection: Arc<RwLock<Value>>,
    entries: Arc<RwLock<Value>>,
    consistency: Arc<RwLock<Value>>,
    task: tokio::task::JoinHandle<()>,
}

impl ToggleRegistry {
    async fn spawn(projection: Value, entries: Value) -> Self {
        let online = Arc::new(AtomicBool::new(true));
        let projection = Arc::new(RwLock::new(projection));
        let entries = Arc::new(RwLock::new(entries));
        let consistency = Arc::new(RwLock::new(json!({})));
        let app = Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let online = Arc::clone(&online);
                    let projection = Arc::clone(&projection);
                    move || {
                        let online = Arc::clone(&online);
                        let projection = Arc::clone(&projection);
                        async move {
                            if !online.load(Ordering::SeqCst) {
                                return StatusCode::SERVICE_UNAVAILABLE.into_response();
                            }
                            Json(projection.read().await.clone()).into_response()
                        }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let online = Arc::clone(&online);
                    let entries = Arc::clone(&entries);
                    move || {
                        let online = Arc::clone(&online);
                        let entries = Arc::clone(&entries);
                        async move {
                            if !online.load(Ordering::SeqCst) {
                                return StatusCode::SERVICE_UNAVAILABLE.into_response();
                            }
                            Json(entries.read().await.clone()).into_response()
                        }
                    }
                }),
            )
            .route(
                "/v1/log/consistency",
                get({
                    let online = Arc::clone(&online);
                    let consistency = Arc::clone(&consistency);
                    move || {
                        let online = Arc::clone(&online);
                        let consistency = Arc::clone(&consistency);
                        async move {
                            if !online.load(Ordering::SeqCst) {
                                return StatusCode::SERVICE_UNAVAILABLE.into_response();
                            }
                            Json(consistency.read().await.clone()).into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            online,
            projection,
            entries,
            consistency,
            task,
        }
    }

    fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::SeqCst);
    }

    async fn replace(&self, projection: Value, entries: Value) {
        *self.projection.write().await = projection;
        *self.entries.write().await = entries;
    }

    async fn replace_with_consistency(
        &self,
        projection: Value,
        entries: Value,
        consistency: Value,
    ) {
        *self.projection.write().await = projection;
        *self.entries.write().await = entries;
        *self.consistency.write().await = consistency;
    }
}

impl Drop for ToggleRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn witnessed_registry_fixture(
    now_secs: u64,
) -> (
    Value,
    Value,
    RegistryTrust,
    ComplianceKeyId,
    ResolvedAttributionKey,
) {
    witnessed_registry_fixture_with_max_age(now_secs, 3_600)
}

fn witnessed_registry_fixture_with_max_age(
    now_secs: u64,
    max_witness_age_secs: u64,
) -> (
    Value,
    Value,
    RegistryTrust,
    ComplianceKeyId,
    ResolvedAttributionKey,
) {
    let operator = SigningKey::from_bytes(&[0x51; 32]);
    let witness = SigningKey::from_bytes(&[0x52; 32]);
    let custodian = Identity::from_seed([0x53; 32]);
    let public_key = keys::x25519_public(&custodian);
    let now_ms = now_secs.saturating_mul(1_000);
    let day_start_ms = now_ms - now_ms % TRACE_EPOCH_DURATION_MS;
    let (key_id, epoch_end_ms) = (0..=31)
        .find_map(|days_back| {
            let epoch_start_ms = day_start_ms.checked_sub(days_back * TRACE_EPOCH_DURATION_MS)?;
            let key_id = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [0x54; 32],
                epoch_start_ms,
                1,
            );
            attribution_epoch_end_ms(&key_id)
                .ok()
                .filter(|end| now_ms < *end)
                .map(|end| (key_id, end))
        })
        .expect("current UTC month has a canonical first day");
    let epoch_start_ms = key_id.epoch_start_ms;
    let publication = ComplianceKeyPublish {
        key_id,
        public_key: hex(&public_key),
        not_before_ms: epoch_start_ms,
        not_after_ms: epoch_end_ms,
        status: ComplianceKeyStatus::Active,
    };
    let entry = LogEntry::compliance_key(0, publication.clone(), epoch_start_ms);
    let mut log = MerkleLog::new();
    log.append(&entry.leaf_bytes().unwrap());
    let checkpoint = Checkpoint {
        origin: "registry.test/log".into(),
        size: 1,
        root: log.root(),
    };
    let mut note = checkpoint.sign(&operator);
    note.push_str(
        &checkpoint
            .cosignature_line("independent", &witness, now_secs)
            .unwrap(),
    );
    let projection = json!({
        "tree_size": 1,
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        "keys": [{
            "key_id_hex": hex(&key_id.encode().unwrap()),
            "publication": publication,
            "log_index": 0,
            "inclusion_path": [],
            "entry": entry,
        }],
    });
    let entries = json!({
        "from": 0,
        "to": 1,
        "tree_size": 1,
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        "entries": [entry],
    });
    let trust = RegistryTrust::new(
        "registry.test/log",
        operator.verifying_key().to_bytes(),
        vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
        1,
        CheckpointPin {
            size: 0,
            root: MerkleLog::new().root(),
        },
        max_witness_age_secs,
        max_witness_age_secs.min(60),
    )
    .unwrap();
    (
        projection,
        entries,
        trust,
        key_id,
        ResolvedAttributionKey {
            public_key,
            not_before_ms: epoch_start_ms,
            not_after_ms: epoch_end_ms,
            status: ComplianceKeyStatus::Active,
        },
    )
}

fn witnessed_registry_fixture_with_two_authorities(
    now_secs: u64,
) -> (
    Value,
    Value,
    RegistryTrust,
    ComplianceKeyId,
    ResolvedAttributionKey,
    AttributionRequirement,
) {
    let operator = SigningKey::from_bytes(&[0x51; 32]);
    let witness = SigningKey::from_bytes(&[0x52; 32]);
    let now_ms = now_secs.saturating_mul(1_000);
    let day_start_ms = now_ms - now_ms % TRACE_EPOCH_DURATION_MS;
    let epoch_start_ms = (0..=31)
        .find_map(|days_back| {
            let epoch_start_ms = day_start_ms.checked_sub(days_back * TRACE_EPOCH_DURATION_MS)?;
            let candidate = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [0x54; 32],
                epoch_start_ms,
                1,
            );
            attribution_epoch_end_ms(&candidate)
                .ok()
                .filter(|end| now_ms < *end)
                .map(|_| epoch_start_ms)
        })
        .expect("current UTC month has a canonical first day");

    let authorities = [([0x54; 32], [0x53; 32]), ([0x55; 32], [0x56; 32])];
    let mut rows = Vec::new();
    let mut log = MerkleLog::new();
    for (index, (authority, seed)) in authorities.into_iter().enumerate() {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            authority,
            epoch_start_ms,
            1,
        );
        let epoch_end_ms = attribution_epoch_end_ms(&key_id).unwrap();
        let public_key = keys::x25519_public(&Identity::from_seed(seed));
        let publication = ComplianceKeyPublish {
            key_id,
            public_key: hex(&public_key),
            not_before_ms: epoch_start_ms,
            not_after_ms: epoch_end_ms,
            status: ComplianceKeyStatus::Active,
        };
        let entry = LogEntry::compliance_key(index as u64, publication.clone(), epoch_start_ms);
        log.append(&entry.leaf_bytes().unwrap());
        rows.push((key_id, public_key, publication, entry));
    }

    let checkpoint = Checkpoint {
        origin: "registry.test/log".into(),
        size: rows.len() as u64,
        root: log.root(),
    };
    let mut note = checkpoint.sign(&operator);
    note.push_str(
        &checkpoint
            .cosignature_line("independent", &witness, now_secs)
            .unwrap(),
    );
    let projection_rows = rows
        .iter()
        .enumerate()
        .map(|(index, (key_id, _, publication, entry))| {
            json!({
                "key_id_hex": hex(&key_id.encode().unwrap()),
                "publication": publication,
                "log_index": index,
                "inclusion_path": log
                    .inclusion_proof(index as u64, rows.len() as u64)
                    .unwrap()
                    .iter()
                    .map(|hash| hex(hash))
                    .collect::<Vec<_>>(),
                "entry": entry,
            })
        })
        .collect::<Vec<_>>();
    let log_entries = rows
        .iter()
        .map(|(_, _, _, entry)| entry.clone())
        .collect::<Vec<_>>();
    let projection = json!({
        "tree_size": rows.len(),
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        "keys": projection_rows,
    });
    let entries = json!({
        "from": 0,
        "to": rows.len(),
        "tree_size": rows.len(),
        "root": hex(&checkpoint.root),
        "checkpoint": note,
        "entries": log_entries,
    });
    let trust = RegistryTrust::new(
        "registry.test/log",
        operator.verifying_key().to_bytes(),
        vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
        1,
        CheckpointPin {
            size: 0,
            root: MerkleLog::new().root(),
        },
        3_600,
        60,
    )
    .unwrap();
    let (wrong_key_id, wrong_public_key, wrong_publication, _) = &rows[0];
    (
        projection,
        entries,
        trust,
        *wrong_key_id,
        ResolvedAttributionKey {
            public_key: *wrong_public_key,
            not_before_ms: wrong_publication.not_before_ms,
            not_after_ms: wrong_publication.not_after_ms,
            status: ComplianceKeyStatus::Active,
        },
        AttributionRequirement::new(Jurisdiction::Test, [0x55; 32]),
    )
}

struct GrowingAttributionFixture {
    old_projection: Value,
    old_entries: Value,
    new_projection: Value,
    new_entries: Value,
    consistency: Value,
    trust: RegistryTrust,
    old_key_id: ComplianceKeyId,
    old_key: ResolvedAttributionKey,
    new_key_id: ComplianceKeyId,
    new_key: ResolvedAttributionKey,
}

fn witnessed_registry_fixture_with_later_generation(now_secs: u64) -> GrowingAttributionFixture {
    let operator = SigningKey::from_bytes(&[0x71; 32]);
    let witness = SigningKey::from_bytes(&[0x72; 32]);
    let old_custodian = Identity::from_seed([0x73; 32]);
    let new_custodian = Identity::from_seed([0x74; 32]);
    let old_public_key = keys::x25519_public(&old_custodian);
    let new_public_key = keys::x25519_public(&new_custodian);
    let now_ms = now_secs.saturating_mul(1_000);
    let day_start_ms = now_ms - now_ms % TRACE_EPOCH_DURATION_MS;
    let epoch_start_ms = (0..=31)
        .find_map(|days_back| {
            let epoch_start_ms = day_start_ms.checked_sub(days_back * TRACE_EPOCH_DURATION_MS)?;
            let candidate = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [0x54; 32],
                epoch_start_ms,
                1,
            );
            attribution_epoch_end_ms(&candidate)
                .ok()
                .filter(|end| now_ms < *end)
                .map(|_| epoch_start_ms)
        })
        .expect("current UTC month has a canonical first day");
    let old_key_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0x54; 32],
        epoch_start_ms,
        1,
    );
    let new_key_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0x54; 32],
        epoch_start_ms,
        2,
    );
    let epoch_end_ms = attribution_epoch_end_ms(&old_key_id).unwrap();
    let old_active = ComplianceKeyPublish {
        key_id: old_key_id,
        public_key: hex(&old_public_key),
        not_before_ms: epoch_start_ms,
        not_after_ms: epoch_end_ms,
        status: ComplianceKeyStatus::Active,
    };
    let mut old_retired = old_active.clone();
    old_retired.status = ComplianceKeyStatus::Retired;
    let new_active = ComplianceKeyPublish {
        key_id: new_key_id,
        public_key: hex(&new_public_key),
        not_before_ms: epoch_start_ms,
        not_after_ms: epoch_end_ms,
        status: ComplianceKeyStatus::Active,
    };
    let log_entries = vec![
        LogEntry::compliance_key(0, old_active.clone(), now_ms),
        LogEntry::compliance_key(1, old_retired.clone(), now_ms.saturating_add(1)),
        LogEntry::compliance_key(2, new_active.clone(), now_ms.saturating_add(2)),
    ];
    let mut log = MerkleLog::new();
    log.append(&log_entries[0].leaf_bytes().unwrap());
    let old_checkpoint = Checkpoint {
        origin: "registry.test/log".into(),
        size: 1,
        root: log.root(),
    };
    for entry in &log_entries[1..] {
        log.append(&entry.leaf_bytes().unwrap());
    }
    let new_checkpoint = Checkpoint {
        origin: "registry.test/log".into(),
        size: 3,
        root: log.root(),
    };
    let mut old_note = old_checkpoint.sign(&operator);
    old_note.push_str(
        &old_checkpoint
            .cosignature_line("independent", &witness, now_secs)
            .unwrap(),
    );
    let mut new_note = new_checkpoint.sign(&operator);
    new_note.push_str(
        &new_checkpoint
            .cosignature_line("independent", &witness, now_secs)
            .unwrap(),
    );

    let old_projection = json!({
        "tree_size": 1,
        "root": hex(&old_checkpoint.root),
        "checkpoint": old_note,
        "keys": [{
            "key_id_hex": hex(&old_key_id.encode().unwrap()),
            "publication": old_active,
            "log_index": 0,
            "inclusion_path": [],
            "entry": log_entries[0],
        }],
    });
    let old_entries = json!({
        "from": 0,
        "to": 1,
        "tree_size": 1,
        "root": hex(&old_checkpoint.root),
        "checkpoint": old_note,
        "entries": [log_entries[0]],
    });
    let new_projection = json!({
        "tree_size": 3,
        "root": hex(&new_checkpoint.root),
        "checkpoint": new_note,
        "keys": [{
            "key_id_hex": hex(&old_key_id.encode().unwrap()),
            "publication": old_retired,
            "log_index": 1,
            "inclusion_path": log.inclusion_proof(1, 3).unwrap()
                .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
            "entry": log_entries[1],
        }, {
            "key_id_hex": hex(&new_key_id.encode().unwrap()),
            "publication": new_active,
            "log_index": 2,
            "inclusion_path": log.inclusion_proof(2, 3).unwrap()
                .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
            "entry": log_entries[2],
        }],
    });
    let new_entries = json!({
        "from": 1,
        "to": 3,
        "tree_size": 3,
        "root": hex(&new_checkpoint.root),
        "checkpoint": new_note,
        "entries": [log_entries[1], log_entries[2]],
    });
    let consistency = json!({
        "from": 1,
        "to": 3,
        "root": hex(&new_checkpoint.root),
        "path": log.consistency_proof(1, 3).unwrap()
            .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
    });
    let trust = RegistryTrust::new(
        "registry.test/log",
        operator.verifying_key().to_bytes(),
        vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
        1,
        CheckpointPin {
            size: 0,
            root: MerkleLog::new().root(),
        },
        3_600,
        60,
    )
    .unwrap();
    GrowingAttributionFixture {
        old_projection,
        old_entries,
        new_projection,
        new_entries,
        consistency,
        trust,
        old_key_id,
        old_key: ResolvedAttributionKey {
            public_key: old_public_key,
            not_before_ms: epoch_start_ms,
            not_after_ms: epoch_end_ms,
            status: ComplianceKeyStatus::Active,
        },
        new_key_id,
        new_key: ResolvedAttributionKey {
            public_key: new_public_key,
            not_before_ms: epoch_start_ms,
            not_after_ms: epoch_end_ms,
            status: ComplianceKeyStatus::Active,
        },
    }
}

#[tokio::test]
async fn attributed_messages_verify_online_and_after_registry_goes_offline() {
    let root = tempfile::tempdir().unwrap();
    let now_secs = now();
    let (projection, entries, trust, key_id, key) = witnessed_registry_fixture(now_secs);
    let (loft, store) = spawn_loft(Some(FixedResolver { key_id, key })).await;
    let (registry_url, shutdown) = spawn_registry(projection, entries).await;

    let alice_home = root.path().join("alice");
    let bob_home = root.path().join("bob");
    let alice = Agent::open(&alice_home).unwrap();
    let bob = Agent::open(&bob_home).unwrap();
    alice.add_loft(&loft).await.unwrap();
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&alice.verifying_key().to_bytes(), "test setup")
        .unwrap();
    alice
        .configure_registry(&registry_url, trust.clone())
        .unwrap();
    bob.configure_registry(&registry_url, trust.clone())
        .unwrap();

    // Model a stale or hostile loft that admitted events before learning the recipient's stronger
    // policy. Recipient-side enforcement must not trust the loft to have applied the gate.
    let attacker = Identity::from_seed([0x61; 32]);
    let absent = envelope::wrap(
        &attacker,
        &bob.verifying_key(),
        "missing attribution from a stale loft",
        now_secs,
    )
    .unwrap();
    let wrong_custodian = Identity::from_seed([0x62; 32]);
    let wrong_public_key = keys::x25519_public(&wrong_custodian);
    let invalid = envelope::wrap_attributed(
        &attacker,
        &bob.verifying_key(),
        "wrongly escrowed by a hostile loft",
        now_secs,
        &wrong_public_key,
        &key_id,
    )
    .unwrap();
    let observed_policy_seq = store
        .policy(bob.verifying_key().as_bytes())
        .unwrap()
        .map(|policy| policy.seq);
    for wrap in [&absent, &invalid] {
        assert!(store
            .admit(
                wrap,
                &wrap.id(),
                now_secs,
                now_secs + 3_600,
                10 * 1024 * 1024,
                observed_policy_seq,
            )
            .unwrap());
    }

    bob.set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();

    let rejected = bob.drain().await.unwrap();
    assert_eq!(rejected.fetched, 2);
    assert_eq!(rejected.dropped, 2);
    assert_eq!(rejected.new_messages, 0);
    assert!(bob.inbox(false, 10).unwrap().is_empty());
    let after_rejection = bob.drain().await.unwrap();
    assert_eq!(
        after_rejection.fetched, 0,
        "rejected events must not pin the cursor"
    );

    let omitted = alice
        .send(&bob.address(), "unattributed omission must be refused")
        .await
        .unwrap_err();
    assert!(omitted.to_string().contains("explicitly configure"));
    assert_eq!(alice.sender_attribution_requirement().unwrap(), None);
    alice
        .send_with_attribution_agreement(
            &bob.address(),
            "witnessed while online",
            Some(test_requirement()),
        )
        .await
        .unwrap();
    assert_eq!(
        alice.sender_attribution_requirement().unwrap(),
        None,
        "call-local agreement must not mutate the persistent sender default"
    );
    bob.drain().await.unwrap();
    let first = bob.inbox(false, 10).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attribution, Attribution::Valid);

    shutdown.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    drop(alice);
    drop(bob);
    let alice = Agent::open(&alice_home).unwrap();
    let bob = Agent::open(&bob_home).unwrap();
    alice
        .send_with_attribution_agreement(
            &bob.address(),
            "verified from the fresh cache",
            Some(test_requirement()),
        )
        .await
        .unwrap();
    bob.drain().await.unwrap();
    let after_outage = bob.inbox(false, 10).unwrap();
    assert_eq!(after_outage.len(), 2);
    assert!(after_outage
        .iter()
        .all(|message| message.attribution == Attribution::Valid));

    bob.set_attribution_requirement(None).await.unwrap();
    alice.set_sender_attribution_requirement(None).unwrap();
    alice
        .send(&bob.address(), "privacy-first mode")
        .await
        .unwrap();
    bob.drain().await.unwrap();
    assert!(bob
        .inbox(false, 10)
        .unwrap()
        .iter()
        .any(|message| message.body.as_str() == "privacy-first mode"
            && message.attribution == Attribution::Absent));

    let charlie = Agent::open(&root.path().join("charlie")).unwrap();
    charlie.add_loft(&loft).await.unwrap();
    charlie.configure_registry(&registry_url, trust).unwrap();
    charlie
        .set_sender_attribution_requirement(Some(test_requirement()))
        .unwrap();
    let error = charlie
        .send(&bob.address(), "must not silently downgrade")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("witnessed"));
}

#[tokio::test]
async fn recipient_drops_valid_attribution_from_a_different_signed_scope() {
    let root = tempfile::tempdir().unwrap();
    let now_secs = now();
    let (projection, entries, trust, key_id, key, required_scope) =
        witnessed_registry_fixture_with_two_authorities(now_secs);
    let (loft, store) = spawn_loft(Some(FixedResolver { key_id, key })).await;
    let (registry_url, _shutdown) = spawn_registry(projection, entries).await;
    let bob = Agent::open(&root.path().join("bob-wrong-scope")).unwrap();
    let sender = Identity::from_seed([0x63; 32]);
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&sender.verifying_key().to_bytes(), "test setup")
        .unwrap();
    bob.configure_registry(&registry_url, trust).unwrap();
    bob.set_attribution_requirement(Some(required_scope))
        .await
        .unwrap();
    bob.refresh_compliance_keys().await.unwrap();

    // Bypass the Loft handler to model a stale/hostile replica. The block is cryptographically
    // valid under a witnessed Active key, but not under Bob's signed authority requirement.
    let wrap = envelope::wrap_attributed(
        &sender,
        &bob.verifying_key(),
        "valid key, wrong authority",
        now_secs,
        &key.public_key,
        &key_id,
    )
    .unwrap();
    let policy_seq = store
        .policy(bob.verifying_key().as_bytes())
        .unwrap()
        .map(|policy| policy.seq);
    assert!(store
        .admit(
            &wrap,
            &wrap.id(),
            now_secs,
            now_secs + 3_600,
            10 * 1024 * 1024,
            policy_seq,
        )
        .unwrap());

    let drained = bob.drain().await.unwrap();
    assert_eq!(
        (drained.fetched, drained.dropped, drained.new_messages),
        (1, 1, 0)
    );
    assert!(bob.inbox(false, 10).unwrap().is_empty());
}

#[tokio::test]
async fn delayed_outbox_wrap_becomes_terminal_when_its_key_retires_before_admission() {
    let root = tempfile::tempdir().unwrap();
    let now_secs = now();
    let (_, _, _, key_id, key) = witnessed_registry_fixture(now_secs);
    let status = Arc::new(AtomicU8::new(1));
    let resolver = Arc::new(TransitionResolver {
        key_id,
        public_key: key.public_key,
        not_before_ms: key.not_before_ms,
        not_after_ms: key.not_after_ms,
        status: Arc::clone(&status),
    });
    let (loft, _) = spawn_loft_with_resolver(Some(resolver.clone())).await;
    let recipient = Identity::from_seed([0x64; 32]);
    LoftClient::new(&loft)
        .unwrap()
        .set_policy(&RecipientPolicy::with_attribution_requirement(
            &recipient,
            0,
            false,
            vec![],
            1,
            Some(test_requirement()),
        ))
        .await
        .unwrap();

    // The immutable signed wrap is queued while its witnessed key is Active, but no request has
    // reached the Loft yet. Retirement before the eventual retry must not rewrite or re-escrow it.
    let sender = Identity::from_seed([0x65; 32]);
    let wrap = envelope::wrap_attributed(
        &sender,
        &recipient.verifying_key(),
        "queued across key retirement",
        now_secs,
        &key.public_key,
        &key_id,
    )
    .unwrap();
    let outbox = Agent::open(&root.path().join("delayed-outbox")).unwrap();
    let message_id = hex(&wrap.id());
    outbox
        .state()
        .queue(
            &message_id,
            recipient.address().as_str(),
            OutboxRoute::new(&loft, true),
            &wrap,
            None,
            now_secs,
        )
        .unwrap();
    resolver.retire();

    let first = outbox.flush().await.unwrap();
    assert_eq!(
        (first.attempted, first.delivered, first.terminal),
        (1, 0, 1)
    );
    assert_eq!(first.queued, 0);
    let dead = outbox.dead_letters(10).unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].message_id, message_id);
    assert_eq!(dead[0].reason, "http_400");

    let second = outbox.flush().await.unwrap();
    assert_eq!(
        second.attempted, 0,
        "terminal debt is never retried or rewritten"
    );
    assert_eq!(second.dead_letters, 1);
}

#[tokio::test]
async fn temporary_loft_attribution_unavailability_preserves_and_then_delivers_queued_wrap() {
    let root = tempfile::tempdir().unwrap();
    let now_secs = now();
    let (_, _, _, key_id, key) = witnessed_registry_fixture(now_secs);
    let resolver = Arc::new(AvailabilityResolver {
        key_id,
        key,
        available: Arc::new(AtomicBool::new(true)),
    });
    let (loft, store) = spawn_loft_with_resolver(Some(resolver.clone())).await;
    let recipient = Identity::from_seed([0x66; 32]);
    LoftClient::new(&loft)
        .unwrap()
        .set_policy(&RecipientPolicy::with_attribution_requirement(
            &recipient,
            0,
            false,
            vec![],
            1,
            Some(test_requirement()),
        ))
        .await
        .unwrap();

    let sender = Identity::from_seed([0x67; 32]);
    let wrap = envelope::wrap_attributed(
        &sender,
        &recipient.verifying_key(),
        "queued across temporary resolver outage",
        now_secs,
        &key.public_key,
        &key_id,
    )
    .unwrap();
    let outbox = Agent::open(&root.path().join("resolver-outage-outbox")).unwrap();
    let message_id = hex(&wrap.id());
    outbox
        .state()
        .queue(
            &message_id,
            recipient.address().as_str(),
            OutboxRoute::new(&loft, true),
            &wrap,
            None,
            now_secs,
        )
        .unwrap();

    resolver.set_available(false);
    let unavailable = outbox.flush().await.unwrap();
    assert_eq!(
        (
            unavailable.attempted,
            unavailable.delivered,
            unavailable.retryable,
            unavailable.terminal,
            unavailable.queued,
            unavailable.dead_letters,
        ),
        (1, 0, 1, 0, 1, 0),
        "Loft attribution readiness failures must be HTTP 503 and remain retryable"
    );
    assert!(store
        .fetch(recipient.verifying_key().as_bytes(), 0, 10)
        .unwrap()
        .is_empty());

    resolver.set_available(true);
    // The first durable retry delay is five seconds plus at most one second of stable jitter.
    tokio::time::sleep(std::time::Duration::from_secs(7)).await;
    let recovered = outbox.flush().await.unwrap();
    assert_eq!(
        (
            recovered.attempted,
            recovered.delivered,
            recovered.terminal,
            recovered.queued,
            recovered.dead_letters,
        ),
        (1, 1, 0, 0, 0)
    );
    assert_eq!(
        store
            .fetch(recipient.verifying_key().as_bytes(), 0, 10)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn stale_attribution_cache_and_registry_outage_preserve_cursor_until_exactly_once_retry() {
    const MAX_WITNESS_AGE_SECS: u64 = 2;

    let root = tempfile::tempdir().unwrap();
    let initial_at = now();
    let (projection, entries, trust, key_id, key) =
        witnessed_registry_fixture_with_max_age(initial_at, MAX_WITNESS_AGE_SECS);
    let (loft, store) = spawn_loft(Some(FixedResolver { key_id, key })).await;
    let registry = ToggleRegistry::spawn(projection, entries).await;

    let alice = Agent::open(&root.path().join("alice-stale")).unwrap();
    let bob = Agent::open(&root.path().join("bob-stale")).unwrap();
    alice.add_loft(&loft).await.unwrap();
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&alice.verifying_key().to_bytes(), "test setup")
        .unwrap();
    alice
        .configure_registry(&registry.url, trust.clone())
        .unwrap();
    bob.configure_registry(&registry.url, trust).unwrap();
    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(now(), MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    bob.set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();
    alice
        .set_sender_attribution_requirement(Some(test_requirement()))
        .unwrap();

    // Re-sign the same immutable one-leaf log immediately before priming each cache. The registry
    // origin, operator key, witness key, leaf, and Merkle root never change; only the witnessed
    // timestamp moves forward.
    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(now(), MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    alice.refresh_compliance_keys().await.unwrap();

    let bob_witnessed_at = now();
    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(bob_witnessed_at, MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    bob.refresh_compliance_keys().await.unwrap();

    let sent = alice
        .send(&bob.address(), "survives stale attribution cache")
        .await
        .unwrap();
    assert_eq!((sent.delivered, sent.queued, sent.terminal), (1, 0, 0));
    assert_eq!(
        store
            .fetch(bob.verifying_key().as_bytes(), 0, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(bob.state().cursor(&loft, &bob.address()).unwrap(), 0);

    registry.set_online(false);
    let stale_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    while now() <= bob_witnessed_at.saturating_add(MAX_WITNESS_AGE_SECS) {
        assert!(
            tokio::time::Instant::now() < stale_deadline,
            "wall clock did not advance far enough to make the witness stale"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let error = bob.drain().await.unwrap_err();
    assert!(
        matches!(&error, ClientError::Registry(_)) || matches!(&error, ClientError::Config(_)),
        "unexpected stale-cache error: {error:?}"
    );
    assert!(
        error.to_string().contains("witness") || error.to_string().contains("stale"),
        "unexpected stale-cache error: {error:?}"
    );
    assert_eq!(bob.state().cursor(&loft, &bob.address()).unwrap(), 0);
    assert!(bob.inbox(false, 10).unwrap().is_empty());
    assert_eq!(
        store
            .fetch(bob.verifying_key().as_bytes(), 0, 10)
            .unwrap()
            .len(),
        1,
        "the loft copy must remain retryable after local trust failure"
    );

    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(now(), MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    registry.set_online(true);
    bob.refresh_compliance_keys().await.unwrap();

    let retry = bob.drain().await.unwrap();
    assert_eq!(retry.fetched, 1);
    assert_eq!(retry.new_messages, 1);
    assert!(bob.state().cursor(&loft, &bob.address()).unwrap() > 0);
    let inbox = bob.inbox(false, 10).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].body.as_str(), "survives stale attribution cache");
    assert_eq!(inbox[0].attribution, Attribution::Valid);

    let after = bob.drain().await.unwrap();
    assert_eq!((after.fetched, after.new_messages), (0, 0));
    assert_eq!(bob.inbox(false, 10).unwrap().len(), 1);
}

#[tokio::test]
async fn older_witnessed_prefix_defers_new_required_key_until_registry_refresh() {
    let root = tempfile::tempdir().unwrap();
    let fixture = witnessed_registry_fixture_with_later_generation(now());
    let registry = ToggleRegistry::spawn(fixture.old_projection, fixture.old_entries).await;
    let (loft, store) = spawn_loft(Some(FixedResolver {
        key_id: fixture.new_key_id,
        key: fixture.new_key,
    }))
    .await;
    let bob = Agent::open(&root.path().join("bob-growing-prefix")).unwrap();
    let sender = Identity::from_seed([0x75; 32]);
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&sender.verifying_key().to_bytes(), "test setup")
        .unwrap();
    bob.configure_registry(&registry.url, fixture.trust)
        .unwrap();
    bob.set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();

    // Bob has a fresh, valid size-one prefix. A size-three checkpoint exists later and contains a
    // replacement generation in the same signed scope, which the Loft already knows as Active.
    let wrap = envelope::wrap_attributed(
        &sender,
        &bob.verifying_key(),
        "new key appended beyond recipient prefix",
        now(),
        &fixture.new_key.public_key,
        &fixture.new_key_id,
    )
    .unwrap();
    LoftClient::new(&loft)
        .unwrap()
        .publish(&wrap, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .fetch(bob.verifying_key().as_bytes(), 0, 10)
            .unwrap()
            .len(),
        1
    );

    registry.set_online(false);
    let unavailable = bob.drain().await.unwrap();
    assert_eq!(
        (
            unavailable.fetched,
            unavailable.dropped,
            unavailable.new_messages,
        ),
        (1, 0, 0)
    );
    assert_eq!(unavailable.lofts_failed, vec![loft.clone()]);
    assert_eq!(bob.state().cursor(&loft, &bob.address()).unwrap(), 0);
    assert!(bob.inbox(false, 10).unwrap().is_empty());
    assert_eq!(
        store
            .fetch(bob.verifying_key().as_bytes(), 0, 10)
            .unwrap()
            .len(),
        1,
        "an older prefix cannot turn a later append into a permanent drop"
    );

    registry
        .replace_with_consistency(
            fixture.new_projection,
            fixture.new_entries,
            fixture.consistency,
        )
        .await;
    registry.set_online(true);
    bob.refresh_compliance_keys().await.unwrap();
    let recovered = bob.drain().await.unwrap();
    assert_eq!(
        (recovered.fetched, recovered.dropped, recovered.new_messages,),
        (1, 0, 1)
    );
    assert!(bob.state().cursor(&loft, &bob.address()).unwrap() > 0);
    let inbox = bob.inbox(false, 10).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0].body.as_str(),
        "new key appended beyond recipient prefix"
    );
    assert_eq!(inbox[0].attribution, Attribution::Valid);
}

#[tokio::test]
async fn one_loft_unknown_key_does_not_starve_a_second_loft_in_the_same_wake() {
    let root = tempfile::tempdir().unwrap();
    let GrowingAttributionFixture {
        old_projection,
        old_entries,
        new_projection,
        new_entries,
        consistency,
        trust,
        old_key_id,
        old_key,
        new_key_id,
        new_key,
    } = witnessed_registry_fixture_with_later_generation(now());
    let registry = ToggleRegistry::spawn(old_projection, old_entries).await;
    let (lagging_loft, _) = spawn_loft(Some(FixedResolver {
        key_id: new_key_id,
        key: new_key,
    }))
    .await;
    let (healthy_loft, _) = spawn_loft(Some(FixedResolver {
        key_id: old_key_id,
        key: old_key,
    }))
    .await;
    let bob = Agent::open(&root.path().join("bob-two-lofts")).unwrap();
    let loft_pubkey = Identity::from_seed([0x41; 32]).verifying_key().to_bytes();
    bob.state()
        .add_loft_with_local_trust(&lagging_loft, Some(loft_pubkey), 1, true)
        .unwrap();
    bob.state()
        .add_loft_with_local_trust(&healthy_loft, Some(loft_pubkey), 2, true)
        .unwrap();
    bob.configure_registry(&registry.url, trust).unwrap();
    bob.set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();

    let sender = Identity::from_seed([0x76; 32]);
    bob.allow_sender(&sender.verifying_key().to_bytes(), "test setup")
        .unwrap();
    let unknown_wrap = envelope::wrap_attributed(
        &sender,
        &bob.verifying_key(),
        "deferred on the first loft",
        now(),
        &new_key.public_key,
        &new_key_id,
    )
    .unwrap();
    LoftClient::new(&lagging_loft)
        .unwrap()
        .publish(&unknown_wrap, None)
        .await
        .unwrap();
    let valid_wrap = envelope::wrap_attributed(
        &sender,
        &bob.verifying_key(),
        "delivered from the second loft",
        now(),
        &old_key.public_key,
        &old_key_id,
    )
    .unwrap();
    LoftClient::new(&healthy_loft)
        .unwrap()
        .publish(&valid_wrap, None)
        .await
        .unwrap();

    registry.set_online(false);
    let limits = WakeupLimits::new(1, std::time::Duration::from_secs(10)).unwrap();
    let first = bob.drain_with_limits(limits).await.unwrap();
    assert_eq!(
        (first.fetched, first.dropped, first.new_messages),
        (2, 0, 1)
    );
    assert_eq!(first.lofts_failed, vec![lagging_loft.clone()]);
    assert_eq!(
        bob.state().cursor(&lagging_loft, &bob.address()).unwrap(),
        0
    );
    assert!(bob.state().cursor(&healthy_loft, &bob.address()).unwrap() > 0);
    let first_inbox = bob.inbox(false, 10).unwrap();
    assert_eq!(first_inbox.len(), 1);
    assert_eq!(
        first_inbox[0].body.as_str(),
        "delivered from the second loft"
    );

    registry
        .replace_with_consistency(new_projection, new_entries, consistency)
        .await;
    registry.set_online(true);
    bob.refresh_compliance_keys().await.unwrap();
    let recovered = bob.drain_with_limits(limits).await.unwrap();
    assert_eq!(
        (recovered.fetched, recovered.new_messages),
        (1, 1),
        "the deferred first route must recover independently"
    );
    assert!(recovered.lofts_failed.is_empty());
    assert_eq!(bob.inbox(false, 10).unwrap().len(), 2);
}

#[tokio::test]
async fn optional_stale_attribution_cannot_pin_plain_messages_behind_it() {
    const MAX_WITNESS_AGE_SECS: u64 = 2;

    let root = tempfile::tempdir().unwrap();
    let initial_at = now();
    let (projection, entries, trust, key_id, key) =
        witnessed_registry_fixture_with_max_age(initial_at, MAX_WITNESS_AGE_SECS);
    let (loft, _) = spawn_loft(Some(FixedResolver { key_id, key })).await;
    let registry = ToggleRegistry::spawn(projection, entries).await;

    let alice = Agent::open(&root.path().join("alice-optional-stale")).unwrap();
    let bob = Agent::open(&root.path().join("bob-optional-stale")).unwrap();
    alice.add_loft(&loft).await.unwrap();
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&alice.verifying_key().to_bytes(), "test setup")
        .unwrap();
    bob.set_attribution_requirement(None).await.unwrap();
    alice
        .configure_registry(&registry.url, trust.clone())
        .unwrap();
    bob.configure_registry(&registry.url, trust).unwrap();
    alice
        .set_sender_attribution_requirement(Some(test_requirement()))
        .unwrap();

    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(now(), MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    alice.refresh_compliance_keys().await.unwrap();

    let bob_witnessed_at = now();
    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(bob_witnessed_at, MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    bob.refresh_compliance_keys().await.unwrap();

    alice
        .send(&bob.address(), "optional stale attribution")
        .await
        .unwrap();
    alice.set_sender_attribution_requirement(None).unwrap();
    alice
        .send(&bob.address(), "plain message behind stale attribution")
        .await
        .unwrap();

    registry.set_online(false);
    let stale_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    while now() <= bob_witnessed_at.saturating_add(MAX_WITNESS_AGE_SECS) {
        assert!(
            tokio::time::Instant::now() < stale_deadline,
            "wall clock did not advance far enough to make the witness stale"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let drained = bob.drain().await.unwrap();
    assert_eq!((drained.fetched, drained.new_messages), (2, 2));
    assert!(bob.state().cursor(&loft, &bob.address()).unwrap() > 0);
    let inbox = bob.inbox(false, 10).unwrap();
    assert_eq!(inbox.len(), 2);
    assert_eq!(
        inbox
            .iter()
            .find(|message| message.body.as_str() == "optional stale attribution")
            .unwrap()
            .attribution,
        Attribution::Invalid
    );
    assert_eq!(
        inbox
            .iter()
            .find(|message| message.body.as_str() == "plain message behind stale attribution")
            .unwrap()
            .attribution,
        Attribution::Absent
    );

    let after = bob.drain().await.unwrap();
    assert_eq!((after.fetched, after.new_messages), (0, 0));
}

#[tokio::test]
async fn optional_stale_attribution_still_rejects_corrupt_cached_key_rows() {
    const MAX_WITNESS_AGE_SECS: u64 = 2;

    let root = tempfile::tempdir().unwrap();
    let initial_at = now();
    let (projection, entries, trust, key_id, key) =
        witnessed_registry_fixture_with_max_age(initial_at, MAX_WITNESS_AGE_SECS);
    let (loft, _) = spawn_loft(Some(FixedResolver { key_id, key })).await;
    let registry = ToggleRegistry::spawn(projection, entries).await;

    let alice = Agent::open(&root.path().join("alice-optional-corrupt")).unwrap();
    let bob_home = root.path().join("bob-optional-corrupt");
    let bob = Agent::open(&bob_home).unwrap();
    alice.add_loft(&loft).await.unwrap();
    bob.add_loft(&loft).await.unwrap();
    bob.allow_sender(&alice.verifying_key().to_bytes(), "test setup")
        .unwrap();
    alice
        .configure_registry(&registry.url, trust.clone())
        .unwrap();
    bob.configure_registry(&registry.url, trust).unwrap();
    alice
        .set_sender_attribution_requirement(Some(test_requirement()))
        .unwrap();

    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(now(), MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    alice.refresh_compliance_keys().await.unwrap();

    let bob_witnessed_at = now();
    let (projection, entries, _, refreshed_key_id, _) =
        witnessed_registry_fixture_with_max_age(bob_witnessed_at, MAX_WITNESS_AGE_SECS);
    assert_eq!(refreshed_key_id, key_id);
    registry.replace(projection, entries).await;
    bob.refresh_compliance_keys().await.unwrap();
    alice
        .send(&bob.address(), "stale block over corrupt cache")
        .await
        .unwrap();

    let state = Connection::open(bob_home.join("state.db")).unwrap();
    assert_eq!(
        state
            .execute("UPDATE compliance_keys SET public_key = zeroblob(32)", [])
            .unwrap(),
        1
    );
    drop(state);
    registry.set_online(false);
    let stale_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    while now() <= bob_witnessed_at.saturating_add(MAX_WITNESS_AGE_SECS) {
        assert!(
            tokio::time::Instant::now() < stale_deadline,
            "wall clock did not advance far enough to make the witness stale"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let error = bob.drain().await.unwrap_err();
    assert!(matches!(error, ClientError::Config(_)), "{error:?}");
    assert!(error.to_string().contains("malformed"), "{error:?}");
    assert_eq!(bob.state().cursor(&loft, &bob.address()).unwrap(), 0);
    assert!(bob.inbox(false, 10).unwrap().is_empty());
}

#[tokio::test]
async fn recipient_attribution_updates_preserve_every_other_signed_policy_field() {
    let root = tempfile::tempdir().unwrap();
    let (projection, entries, trust, _, _) = witnessed_registry_fixture(now());
    let (registry_url, _shutdown) = spawn_registry(projection, entries).await;
    let (loft, store) = spawn_loft(None).await;
    let agent = Agent::open(&root.path().join("agent")).unwrap();
    agent.add_loft(&loft).await.unwrap();
    agent.configure_registry(&registry_url, trust).unwrap();
    agent.set_pow_floor(7).await.unwrap();
    agent.publish_token("project-room").await.unwrap();

    agent
        .set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();
    let required = store
        .policy(agent.verifying_key().as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(required.pow_min, 7);
    assert!(required.token_required);
    assert_eq!(required.token_hashes.len(), 1);
    assert!(required.attribution_required);

    agent.set_attribution_requirement(None).await.unwrap();
    let optional = store
        .policy(agent.verifying_key().as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(optional.pow_min, 7);
    assert!(optional.token_required);
    assert_eq!(optional.token_hashes, required.token_hashes);
    assert!(!optional.attribution_required);
    assert!(optional.seq > required.seq);
}

#[tokio::test]
async fn draining_lofts_receive_complete_policy_until_expiry_without_being_revived() {
    let root = tempfile::tempdir().unwrap();
    let (projection, entries, trust, _, _) = witnessed_registry_fixture(now());
    let (registry_url, _shutdown) = spawn_registry(projection, entries).await;
    let (draining_url, draining_store) = spawn_loft(None).await;
    let agent = Agent::open(&root.path().join("draining")).unwrap();
    agent.add_loft(&draining_url).await.unwrap();
    agent
        .configure_registry(&registry_url, trust.clone())
        .unwrap();
    assert!(agent.remove_loft(&draining_url).await.unwrap());
    assert!(agent.lofts().unwrap().is_empty());

    // Newly resolving senders no longer see this route, but stale senders may still deposit there.
    // Every admission field therefore has to follow the recipient through the full drain grace.
    agent.set_pow_floor(11).await.unwrap();
    agent.publish_token("drain-grace").await.unwrap();
    agent
        .set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();
    let during_grace = draining_store
        .policy(agent.verifying_key().as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(during_grace.pow_min, 11);
    assert!(during_grace.token_required);
    assert_eq!(during_grace.token_hashes.len(), 1);
    assert!(during_grace.attribution_required);

    // Build a route whose grace deadline is already in the past. The policy query itself must
    // finalize it before attempting transport, and later settings must not recreate or contact it.
    let (expired_url, expired_store) = spawn_loft(None).await;
    let expired = Agent::open(&root.path().join("expired")).unwrap();
    expired.add_loft(&expired_url).await.unwrap();
    expired.configure_registry(&registry_url, trust).unwrap();
    let before_expiry = expired_store
        .policy(expired.verifying_key().as_bytes())
        .unwrap()
        .unwrap();
    expired.state().remove_loft(&expired_url, 0).unwrap();

    expired.set_pow_floor(13).await.unwrap();
    expired.publish_token("must-not-revive").await.unwrap();
    expired
        .set_attribution_requirement(Some(test_requirement()))
        .await
        .unwrap();
    let after_expiry = expired_store
        .policy(expired.verifying_key().as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(after_expiry, before_expiry);
    assert!(expired.lofts().unwrap().is_empty());
    assert!(expired
        .state()
        .lofts_for_drain_with_local_trust(now())
        .unwrap()
        .is_empty());
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn test_requirement() -> AttributionRequirement {
    AttributionRequirement::new(Jurisdiction::Test, [0x54; 32])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
