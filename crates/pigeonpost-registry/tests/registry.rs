//! Registry acceptance, migration, persistence, concurrency, compliance-key, and HTTP tests.

#![cfg(feature = "test-utilities")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ed25519_dalek::{Signer, SigningKey};
#[cfg(feature = "test-utilities")]
use pigeonpost_compliance_format::COMPLIANCE_KEY_ID_LEN;
use pigeonpost_compliance_format::{
    attribution_epoch_end_ms, ComplianceKeyId, CompliancePurpose, Jurisdiction, TraceCapturePolicy,
    TraceRetentionPolicy, TRACE_EPOCH_DURATION_MS,
};
use pigeonpost_core::Identity;
#[cfg(feature = "test-utilities")]
use pigeonpost_registry::directory_publisher::{
    mutation_request_bytes, DirectoryMutationOperation, DIRECTORY_PUBLISHER_KEY_HEADER,
    DIRECTORY_PUBLISHER_SIGNATURE_HEADER,
};
#[cfg(unix)]
use pigeonpost_registry::MerkleLog;
use pigeonpost_registry::{
    claim_trace::{ClaimTraceCapacity, ClaimTraceError, ClaimTraceInput, ClaimTraceSink},
    entry::{
        claim_payload, directory_add_claim_payload, directory_remove_claim_payload,
        ComplianceKeyPublish, ComplianceKeyStatus, DirectoryAdd, DirectoryRemove, EntryKind,
        LogEntry,
    },
    identity::{MockProvider, ProofPayload},
    log::{self, verify_consistency, verify_inclusion},
    Checkpoint, ComplianceKeyQuery, Handle, RegistrationLimits, Registry, RegistryConfig,
    RegistryError, WitnessKey, WitnessPolicy,
};
#[cfg(unix)]
use rusqlite::{params, Connection};

const ORIGIN: &str = "pigeonpost.dev/registry";

#[cfg(feature = "test-utilities")]
fn test_directory_publisher() -> SigningKey {
    SigningKey::from_bytes(&[88; 32])
}

#[cfg(feature = "test-utilities")]
fn test_http_config() -> pigeonpost_registry::RegistryHttpConfig {
    pigeonpost_registry::RegistryHttpConfig::direct()
        .with_directory_publishers(vec![test_directory_publisher().verifying_key()])
        .unwrap()
}

#[cfg(feature = "test-utilities")]
fn directory_auth_headers(
    operation: DirectoryMutationOperation,
    body: &[u8],
) -> [(&'static str, String); 2] {
    let publisher = test_directory_publisher();
    let request = mutation_request_bytes(ORIGIN, operation, body).unwrap();
    [
        (
            DIRECTORY_PUBLISHER_KEY_HEADER,
            pigeonpost_registry::registry::hex(publisher.verifying_key().as_bytes()),
        ),
        (
            DIRECTORY_PUBLISHER_SIGNATURE_HEADER,
            pigeonpost_registry::registry::hex(&publisher.sign(&request).to_bytes()),
        ),
    ]
}

fn config() -> RegistryConfig {
    RegistryConfig {
        origin: ORIGIN.into(),
        signing_key: SigningKey::from_bytes(&[42u8; 32]),
        allow_mock_identities: true,
    }
}

fn registry() -> Registry {
    with_test_trace(
        Registry::in_memory(config())
            .unwrap()
            .with_provider(Box::new(MockProvider)),
    )
}

#[cfg(unix)]
fn secure_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(directory.path()).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(directory.path(), permissions).unwrap();
    }
    directory
}

#[derive(Debug)]
struct TestClaimTraceSink;

impl ClaimTraceSink for TestClaimTraceSink {
    fn capacity_contract(&self) -> Option<ClaimTraceCapacity> {
        Some(ClaimTraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Test,
                capture: TraceCapturePolicy::Standing,
                retention_days: None,
            },
            records_per_minute: 10_000,
            utc_epochs: 1,
            max_records_per_segment: 10_000,
            network_logical_limit_bytes: pigeonpost_compliance_seal::MAX_TRACE_STORAGE_BYTES,
            identity_logical_limit_bytes: pigeonpost_compliance_seal::MAX_TRACE_STORAGE_BYTES,
        })
    }

    fn readiness(&self, _now_ms: u64) -> Result<(), ClaimTraceError> {
        Ok(())
    }

    fn capture(&self, input: ClaimTraceInput) -> Result<(), ClaimTraceError> {
        assert!(!input.provider_subject.is_empty());
        assert_ne!(input.source.port(), 0);
        Ok(())
    }

    fn shutdown(&self, _timestamp_ms: u64) -> Result<(), ClaimTraceError> {
        Ok(())
    }
}

#[derive(Debug)]
struct CountingClaimTraceSink {
    captures: AtomicUsize,
    fail: bool,
    records_per_minute: u32,
}

impl CountingClaimTraceSink {
    fn new(fail: bool) -> Self {
        Self {
            captures: AtomicUsize::new(0),
            fail,
            records_per_minute: 10_000,
        }
    }

    fn with_rate(records_per_minute: u32) -> Self {
        Self {
            captures: AtomicUsize::new(0),
            fail: false,
            records_per_minute,
        }
    }
}

impl ClaimTraceSink for CountingClaimTraceSink {
    fn capacity_contract(&self) -> Option<ClaimTraceCapacity> {
        Some(ClaimTraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Test,
                capture: TraceCapturePolicy::Standing,
                retention_days: None,
            },
            records_per_minute: self.records_per_minute,
            utc_epochs: 1,
            max_records_per_segment: 10_000,
            network_logical_limit_bytes: pigeonpost_compliance_seal::MAX_TRACE_STORAGE_BYTES,
            identity_logical_limit_bytes: pigeonpost_compliance_seal::MAX_TRACE_STORAGE_BYTES,
        })
    }

    fn readiness(&self, _now_ms: u64) -> Result<(), ClaimTraceError> {
        Ok(())
    }

    fn capture(&self, _input: ClaimTraceInput) -> Result<(), ClaimTraceError> {
        self.captures.fetch_add(1, Ordering::AcqRel);
        if self.fail {
            Err(ClaimTraceError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn shutdown(&self, _timestamp_ms: u64) -> Result<(), ClaimTraceError> {
        Ok(())
    }
}

fn source() -> SocketAddr {
    "192.0.2.10:4242".parse().unwrap()
}

fn with_test_trace(registry: Registry) -> Registry {
    registry.with_claim_trace(Arc::new(TestClaimTraceSink))
}

fn claim(identity: &Identity, handle: &Handle) -> ([u8; 32], [u8; 64]) {
    let pubkey = identity.verifying_key().to_bytes();
    let signature = identity
        .sign(&claim_payload(&handle.as_path(), &pubkey))
        .to_bytes();
    (pubkey, signature)
}

fn mock(name: &str) -> ProofPayload {
    ProofPayload::Mock { name: name.into() }
}

fn directory_addition(
    key: &SigningKey,
    endpoint: &str,
    sequence: u64,
    capacity_gb: u64,
) -> DirectoryAdd {
    let pubkey = pigeonpost_registry::registry::hex(key.verifying_key().as_bytes());
    let payload = directory_add_claim_payload(
        endpoint,
        &pubkey,
        None,
        capacity_gb,
        30,
        true,
        0,
        65_536,
        sequence,
    )
    .unwrap();
    DirectoryAdd::authenticated(
        endpoint.into(),
        pubkey,
        None,
        capacity_gb,
        30,
        true,
        0,
        65_536,
        sequence,
        pigeonpost_registry::registry::hex(&key.sign(&payload).to_bytes()),
    )
    .unwrap()
}

fn directory_removal(
    key: &SigningKey,
    endpoint: &str,
    after: u64,
    sequence: u64,
) -> DirectoryRemove {
    let payload = directory_remove_claim_payload(endpoint, after, sequence).unwrap();
    DirectoryRemove::authenticated(
        endpoint.into(),
        pigeonpost_registry::registry::hex(key.verifying_key().as_bytes()),
        after,
        sequence,
        pigeonpost_registry::registry::hex(&key.sign(&payload).to_bytes()),
    )
}

async fn add(registry: &Registry, seed: u8, name: &str) {
    let identity = Identity::from_seed([seed; 32]);
    let handle = Handle::parse(&format!("/github/{name}")).unwrap();
    let (pubkey, signature) = claim(&identity, &handle);
    registry
        .register(&handle, &pubkey, &signature, &mock(name), source())
        .await
        .unwrap();
}

#[tokio::test]
async fn claim_resolve_and_strict_dump_verify_without_trusting_the_registry() {
    let registry = registry();
    add(&registry, 1, "alice").await;
    for (seed, name) in [(2, "bob"), (3, "carol"), (4, "dave")] {
        add(&registry, seed, name).await;
    }

    let handle = Handle::parse("/github/alice").unwrap();
    let resolved = registry.resolve(&handle).unwrap();
    let entry = registry.entry(resolved.index).unwrap();
    assert_eq!(entry.kind(), EntryKind::HandleClaim);
    assert_eq!(entry.seq(), resolved.index);
    let leaf = log::leaf_hash(&entry.leaf_bytes().unwrap());
    assert!(verify_inclusion(
        &leaf,
        resolved.index,
        resolved.inclusion.size,
        &resolved.inclusion.path,
        &resolved.inclusion.root,
    ));

    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["type"], "handle_bind");
    assert!(json.get("payload").is_some());
    assert!(json.get("kind").is_none());
    Checkpoint::verify(&resolved.inclusion.checkpoint, &registry.verifying_key()).unwrap();
}

#[tokio::test]
async fn a_forged_entry_does_not_verify() {
    let registry = registry();
    add(&registry, 1, "alice").await;
    let resolved = registry
        .resolve(&Handle::parse("/github/alice").unwrap())
        .unwrap();
    let mut forged = registry.entry(0).unwrap();
    if let LogEntry::HandleClaim(versioned) = &mut forged {
        versioned.payload.pubkey = "00".repeat(32);
    }
    assert!(!verify_inclusion(
        &log::leaf_hash(&forged.leaf_bytes().unwrap()),
        0,
        resolved.inclusion.size,
        &resolved.inclusion.path,
        &resolved.inclusion.root,
    ));
}

#[tokio::test]
async fn identity_name_and_key_possession_are_both_required() {
    let registry = registry();
    let attacker = Identity::from_seed([9; 32]);
    let victim = Identity::from_seed([1; 32]);
    let handle = Handle::parse("/github/alice").unwrap();
    let (attacker_key, attacker_signature) = claim(&attacker, &handle);

    let mismatch = registry
        .register(
            &handle,
            &attacker_key,
            &attacker_signature,
            &mock("mallory"),
            source(),
        )
        .await
        .unwrap_err();
    assert!(matches!(mismatch, RegistryError::SubjectMismatch { .. }));

    let victim_key = victim.verifying_key().to_bytes();
    let wrong_key = registry
        .register(
            &handle,
            &victim_key,
            &attacker_signature,
            &mock("alice"),
            source(),
        )
        .await
        .unwrap_err();
    assert!(matches!(wrong_key, RegistryError::KeyPossessionNotProved));
}

#[tokio::test]
async fn stable_account_budget_cannot_be_evaded_by_rotating_source_addresses() {
    let registry = registry()
        .with_registration_limits(RegistrationLimits {
            account_bindings_per_minute: 1,
            max_account_keys: 8,
            ..RegistrationLimits::default()
        })
        .unwrap();
    let alice_identity = Identity::from_seed([81; 32]);
    let alice = Handle::parse("/github/alice").unwrap();
    let (alice_key, alice_signature) = claim(&alice_identity, &alice);
    registry
        .register(
            &alice,
            &alice_key,
            &alice_signature,
            &mock("alice"),
            "192.0.2.10:4242".parse().unwrap(),
        )
        .await
        .unwrap();

    let error = registry
        .register(
            &alice,
            &alice_key,
            &alice_signature,
            &mock("alice"),
            "198.51.100.77:5151".parse().unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, RegistryError::RateLimited));

    let bob_identity = Identity::from_seed([82; 32]);
    let bob = Handle::parse("/github/bob").unwrap();
    let (bob_key, bob_signature) = claim(&bob_identity, &bob);
    registry
        .register(
            &bob,
            &bob_key,
            &bob_signature,
            &mock("bob"),
            "203.0.113.99:6262".parse().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(registry.size().unwrap(), 2);
}

#[tokio::test]
async fn global_binding_budget_is_exact_under_concurrent_direct_admission() {
    let trace = Arc::new(CountingClaimTraceSink::new(false));
    let registry = Registry::in_memory(config())
        .unwrap()
        .with_provider(Box::new(MockProvider))
        .with_claim_trace(trace.clone())
        .with_registration_limits(RegistrationLimits {
            global_bindings_per_minute: 2,
            account_bindings_per_minute: 10,
            max_account_keys: 8,
        })
        .unwrap();
    let alice = Handle::parse("/github/alice").unwrap();
    let bob = Handle::parse("/github/bob").unwrap();
    let carol = Handle::parse("/github/carol").unwrap();
    let (alice_key, alice_signature) = claim(&Identity::from_seed([83; 32]), &alice);
    let (bob_key, bob_signature) = claim(&Identity::from_seed([84; 32]), &bob);
    let (carol_key, carol_signature) = claim(&Identity::from_seed([85; 32]), &carol);
    let alice_proof = mock("alice");
    let bob_proof = mock("bob");
    let carol_proof = mock("carol");

    let (alice_result, bob_result, carol_result) = tokio::join!(
        registry.register(&alice, &alice_key, &alice_signature, &alice_proof, source()),
        registry.register(&bob, &bob_key, &bob_signature, &bob_proof, source()),
        registry.register(&carol, &carol_key, &carol_signature, &carol_proof, source(),),
    );
    let results = [alice_result, bob_result, carol_result];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 2);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(RegistryError::RateLimited)))
            .count(),
        1
    );
    assert_eq!(trace.captures.load(Ordering::Acquire), 2);
    assert_eq!(registry.size().unwrap(), 2);
}

#[tokio::test]
async fn failed_trace_admission_burns_but_never_refunds_the_global_slot() {
    let trace = Arc::new(CountingClaimTraceSink::new(true));
    let registry = Registry::in_memory(config())
        .unwrap()
        .with_provider(Box::new(MockProvider))
        .with_claim_trace(trace.clone())
        .with_registration_limits(RegistrationLimits {
            global_bindings_per_minute: 1,
            account_bindings_per_minute: 10,
            max_account_keys: 8,
        })
        .unwrap();
    let alice = Handle::parse("/github/alice").unwrap();
    let bob = Handle::parse("/github/bob").unwrap();
    let (alice_key, alice_signature) = claim(&Identity::from_seed([86; 32]), &alice);
    let (bob_key, bob_signature) = claim(&Identity::from_seed([87; 32]), &bob);

    assert!(matches!(
        registry
            .register(
                &alice,
                &alice_key,
                &alice_signature,
                &mock("alice"),
                source()
            )
            .await,
        Err(RegistryError::ClaimTraceUnavailable)
    ));
    assert!(matches!(
        registry
            .register(&bob, &bob_key, &bob_signature, &mock("bob"), source())
            .await,
        Err(RegistryError::RateLimited)
    ));
    assert_eq!(trace.captures.load(Ordering::Acquire), 1);
    assert_eq!(registry.size().unwrap(), 0);
}

#[tokio::test]
async fn direct_registration_rejects_a_trace_contract_below_the_global_rate() {
    let trace = Arc::new(CountingClaimTraceSink::with_rate(1));
    let registry = Registry::in_memory(config())
        .unwrap()
        .with_provider(Box::new(MockProvider))
        .with_claim_trace(trace.clone())
        .with_registration_limits(RegistrationLimits {
            global_bindings_per_minute: 2,
            account_bindings_per_minute: 10,
            max_account_keys: 8,
        })
        .unwrap();
    let alice = Handle::parse("/github/alice").unwrap();
    let (alice_key, alice_signature) = claim(&Identity::from_seed([88; 32]), &alice);

    assert!(matches!(
        registry
            .register(
                &alice,
                &alice_key,
                &alice_signature,
                &mock("alice"),
                source()
            )
            .await,
        Err(RegistryError::InvalidConfiguration(_))
    ));
    assert_eq!(trace.captures.load(Ordering::Acquire), 0);
    assert_eq!(registry.size().unwrap(), 0);
}

#[test]
fn checkpoint_operator_key_cannot_be_configured_as_its_own_witness() {
    let registry = Registry::in_memory(config()).unwrap();
    let witness = WitnessKey::new("operator", registry.verifying_key()).unwrap();
    let policy = WitnessPolicy::new(vec![witness], 1, 600, 30, 0).unwrap();
    let error = registry.with_witness_policy(policy).err().unwrap();
    assert!(matches!(error, RegistryError::InvalidConfiguration(_)));
}

#[tokio::test]
async fn rotation_appends_and_idempotent_retry_does_not() {
    let registry = registry();
    let handle = Handle::parse("/github/alice").unwrap();
    let first = Identity::from_seed([1; 32]);
    let (first_key, first_sig) = claim(&first, &handle);
    let initial = registry
        .register(&handle, &first_key, &first_sig, &mock("alice"), source())
        .await
        .unwrap();
    let retry = registry
        .register(&handle, &first_key, &first_sig, &mock("alice"), source())
        .await
        .unwrap();
    assert_eq!(retry.index, initial.index);
    assert!(!retry.appended);

    let second = Identity::from_seed([2; 32]);
    let (second_key, second_sig) = claim(&second, &handle);
    let conflict = registry
        .register(&handle, &second_key, &second_sig, &mock("alice"), source())
        .await
        .unwrap_err();
    assert!(matches!(conflict, RegistryError::AlreadyBound));
    registry
        .rotate(&handle, &second_key, &second_sig, &mock("alice"), source())
        .await
        .unwrap();
    let entries = registry.dump().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind(), EntryKind::HandleClaim);
    assert_eq!(entries[1].kind(), EntryKind::HandleRotation);
    assert_eq!(
        entries[0].handle_binding().unwrap().1,
        pigeonpost_registry::registry::hex(&first_key)
    );
    assert_eq!(
        registry.resolve(&handle).unwrap().pubkey,
        pigeonpost_registry::registry::hex(&second_key)
    );
}

#[test]
fn directory_mutations_are_authenticated_monotonic_idempotent_and_key_scoped() {
    let registry = registry();
    let endpoint = "https://loft.example";
    let key = SigningKey::from_bytes(&[11; 32]);
    let addition = directory_addition(&key, endpoint, 1, 10);

    let first = registry.append_directory_add(addition.clone()).unwrap();
    assert!(first.appended);
    assert_eq!(first.index, 0);
    assert_eq!(first.entry.directory_addition(), Some(&addition));
    assert!(verify_inclusion(
        &log::leaf_hash(&first.entry.leaf_bytes().unwrap()),
        first.index,
        first.inclusion.size,
        &first.inclusion.path,
        &first.inclusion.root,
    ));

    let retry = registry.append_directory_add(addition).unwrap();
    assert!(!retry.appended);
    assert_eq!(retry.index, first.index);
    assert_eq!(registry.size().unwrap(), 1);

    let equivocation = directory_addition(&key, endpoint, 1, 11);
    assert!(matches!(
        registry.append_directory_add(equivocation),
        Err(RegistryError::DirectoryReplay)
    ));

    let other_key = SigningKey::from_bytes(&[12; 32]);
    assert!(matches!(
        registry.append_directory_add(directory_addition(&other_key, endpoint, 2, 10)),
        Err(RegistryError::DirectoryReplay)
    ));
    let competing = registry
        .append_directory_add(directory_addition(&other_key, endpoint, 1, 10))
        .unwrap();
    assert!(competing.appended);
    assert_eq!(competing.index, 1);
    let competing_retry = registry
        .append_directory_add(directory_addition(&other_key, endpoint, 1, 10))
        .unwrap();
    assert!(!competing_retry.appended);
    assert_eq!(competing_retry.index, competing.index);
    assert!(matches!(
        registry.append_directory_add(directory_addition(&key, endpoint, 0, 10)),
        Err(RegistryError::DirectoryReplay)
    ));

    let removal = directory_removal(&key, endpoint, 2_000_000, 2);
    let removed = registry.append_directory_remove(removal.clone()).unwrap();
    assert!(removed.appended);
    assert_eq!(removed.entry.directory_removal(), Some(&removal));
    assert!(verify_inclusion(
        &log::leaf_hash(&removed.entry.leaf_bytes().unwrap()),
        removed.index,
        removed.inclusion.size,
        &removed.inclusion.path,
        &removed.inclusion.root,
    ));
    let removal_retry = registry.append_directory_remove(removal).unwrap();
    assert!(!removal_retry.appended);
    assert_eq!(removal_retry.index, removed.index);
    assert_eq!(registry.size().unwrap(), 3);
}

#[test]
fn forged_and_out_of_order_directory_mutations_never_append() {
    let registry = registry();
    let key = SigningKey::from_bytes(&[21; 32]);
    let endpoint = "https://loft.example";

    assert!(matches!(
        registry.append_directory_remove(directory_removal(&key, endpoint, 10, 1)),
        Err(RegistryError::NotFound)
    ));
    assert!(matches!(
        registry.append_directory_add(directory_addition(&key, endpoint, 7, 10)),
        Err(RegistryError::DirectoryReplay)
    ));

    let mut forged = directory_addition(&key, endpoint, 1, 10);
    forged.authentication.as_mut().unwrap().loft_signature = "00".repeat(64);
    assert!(matches!(
        registry.append_directory_add(forged),
        Err(RegistryError::MalformedEntry(_))
    ));
    assert_eq!(registry.size().unwrap(), 0);
}

#[tokio::test]
async fn checkpoints_are_persisted_and_consistency_proofs_verify() {
    let registry = registry();
    for (seed, name) in [(1, "a"), (2, "b"), (3, "c"), (4, "d")] {
        add(&registry, seed, name).await;
    }
    let old =
        Checkpoint::verify(&registry.checkpoint().unwrap(), &registry.verifying_key()).unwrap();
    for (seed, name) in [(5, "e"), (6, "f"), (7, "g")] {
        add(&registry, seed, name).await;
    }
    let new =
        Checkpoint::verify(&registry.checkpoint().unwrap(), &registry.verifying_key()).unwrap();
    let (to, root, proof) = registry.consistency_proof(old.size).unwrap().unwrap();
    assert_eq!((to, root), (new.size, new.root));
    assert!(verify_consistency(
        old.size, &old.root, new.size, &new.root, &proof
    ));
}

#[tokio::test]
async fn compliance_keys_are_typed_validated_historical_and_proof_bearing() {
    let registry = registry();
    add(&registry, 1, "alice").await;
    let current = now_ms();
    let day_start = current - current % TRACE_EPOCH_DURATION_MS;
    let (start, end) = (0..=31)
        .find_map(|days_back| {
            let start = day_start.checked_sub(days_back * TRACE_EPOCH_DURATION_MS)?;
            let id = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Eu,
                [7; 32],
                start,
                1,
            );
            attribution_epoch_end_ms(&id)
                .ok()
                .filter(|end| current < *end)
                .map(|end| (start, end))
        })
        .expect("current UTC month has a canonical first day");
    let key_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Eu,
        [7; 32],
        start,
        1,
    );
    let publication = ComplianceKeyPublish {
        key_id,
        public_key: "22".repeat(32),
        not_before_ms: start,
        not_after_ms: end,
        status: ComplianceKeyStatus::Active,
    };
    let mut never_active = publication.clone();
    never_active.status = ComplianceKeyStatus::Retired;
    assert!(matches!(
        registry.publish_compliance_key(never_active),
        Err(RegistryError::MalformedEntry(_))
    ));
    let logged = registry
        .publish_compliance_key(publication.clone())
        .unwrap();
    assert_eq!(logged.index, 1);
    let retry = registry
        .publish_compliance_key_idempotent(publication.clone())
        .unwrap();
    assert!(!retry.appended);
    assert_eq!(retry.key.index, logged.index);
    assert_eq!(registry.size().unwrap(), 2);
    let entry = registry.entry(logged.index).unwrap();
    assert_eq!(entry.kind(), EntryKind::ComplianceKeyPublish);
    assert!(verify_inclusion(
        &log::leaf_hash(&entry.leaf_bytes().unwrap()),
        logged.index,
        logged.inclusion.size,
        &logged.inclusion.path,
        &logged.inclusion.root,
    ));

    let live = registry
        .compliance_keys(&ComplianceKeyQuery::default())
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].publication, publication);
    let metadata = registry
        .compliance_key_set(&ComplianceKeyQuery {
            metadata_only: true,
            ..Default::default()
        })
        .unwrap();
    assert!(metadata.keys.is_empty());
    assert_eq!(metadata.head.size, registry.size().unwrap());
    let historical = registry
        .compliance_keys(&ComplianceKeyQuery {
            key_id: Some(key_id),
            include_inactive: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(historical.len(), 1);

    let mut revoked = publication.clone();
    revoked.status = ComplianceKeyStatus::Revoked;
    let revoked_log = registry.publish_compliance_key(revoked.clone()).unwrap();
    assert!(revoked_log.index > logged.index);
    let revoked_retry = registry
        .publish_compliance_key_idempotent(revoked.clone())
        .unwrap();
    assert!(!revoked_retry.appended);
    assert_eq!(revoked_retry.key.index, revoked_log.index);
    assert!(registry
        .compliance_keys(&ComplianceKeyQuery::default())
        .unwrap()
        .is_empty());
    let latest = registry
        .compliance_keys(&ComplianceKeyQuery {
            key_id: Some(key_id),
            include_inactive: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].index, revoked_log.index);
    assert_eq!(latest[0].publication, revoked);

    let mut invalid = publication;
    invalid.not_after_ms = invalid.not_before_ms + 33 * 24 * 60 * 60 * 1_000;
    assert!(matches!(
        registry.publish_compliance_key(invalid),
        Err(RegistryError::MalformedEntry(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn persisted_nodes_survive_restart_without_rebuilding_the_log() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry.db");
    let path_text = path.to_str().unwrap();
    let root_before;
    {
        let registry = with_test_trace(
            Registry::open(path_text, config())
                .unwrap()
                .with_provider(Box::new(MockProvider)),
        );
        for index in 0..64u8 {
            add(&registry, index.saturating_add(1), &format!("agent{index}")).await;
        }
        assert_eq!(registry.audit_storage().unwrap(), 64);
        root_before = registry.head().unwrap().root;
    }
    let reopened = with_test_trace(
        Registry::open(path_text, config())
            .unwrap()
            .with_provider(Box::new(MockProvider)),
    );
    assert_eq!(reopened.size().unwrap(), 64);
    assert_eq!(reopened.head().unwrap().root, root_before);
    assert_eq!(reopened.audit_storage().unwrap(), 64);

    let connection = Connection::open(&path).unwrap();
    let nodes: i64 = connection
        .query_row("SELECT COUNT(*) FROM merkle_nodes", [], |row| row.get(0))
        .unwrap();
    assert!(nodes < 128, "incremental tree stores fewer than 2n nodes");
    let checkpoints: i64 = connection
        .query_row("SELECT COUNT(*) FROM checkpoints", [], |row| row.get(0))
        .unwrap();
    assert_eq!(checkpoints, 65, "one immutable checkpoint per tree size");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_connections_append_unique_contiguous_sequences_concurrently() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry.db");
    let path_text = path.to_str().unwrap();
    let first = Arc::new(with_test_trace(
        Registry::open(path_text, config())
            .unwrap()
            .with_provider(Box::new(MockProvider)),
    ));
    let second = Arc::new(with_test_trace(
        Registry::open(path_text, config())
            .unwrap()
            .with_provider(Box::new(MockProvider)),
    ));
    let mut tasks = Vec::new();
    for index in 0..40u8 {
        let registry = if index % 2 == 0 {
            Arc::clone(&first)
        } else {
            Arc::clone(&second)
        };
        tasks.push(tokio::spawn(async move {
            add(
                &registry,
                index.saturating_add(1),
                &format!("concurrent{index}"),
            )
            .await;
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(first.size().unwrap(), 40);
    let entries = second.dump().unwrap();
    assert_eq!(entries.len(), 40);
    for (seq, entry) in entries.iter().enumerate() {
        assert_eq!(entry.seq(), seq as u64);
    }
    assert_eq!(first.audit_storage().unwrap(), 40);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compliance_publications_allocate_sequences_inside_the_write_transaction() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry.db");
    let first = Arc::new(Registry::open(path.to_str().unwrap(), config()).unwrap());
    let second = Arc::new(Registry::open(path.to_str().unwrap(), config()).unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(24));
    let start = now_ms() / (24 * 60 * 60 * 1_000) * (24 * 60 * 60 * 1_000);
    let mut tasks = Vec::new();
    for index in 0..24u8 {
        let registry = if index % 2 == 0 {
            Arc::clone(&first)
        } else {
            Arc::clone(&second)
        };
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            registry.publish_compliance_key(ComplianceKeyPublish {
                key_id: ComplianceKeyId::new(
                    CompliancePurpose::NetworkTrace,
                    Jurisdiction::Tr,
                    [index.saturating_add(1); 32],
                    start,
                    u32::from(index) + 1,
                ),
                public_key: format!("{:02x}", index.saturating_add(1)).repeat(32),
                not_before_ms: start,
                not_after_ms: start + 24 * 60 * 60 * 1_000,
                status: ComplianceKeyStatus::Active,
            })
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(first.size().unwrap(), 24);
    assert_eq!(second.audit_storage().unwrap(), 24);
    assert_eq!(
        first
            .compliance_keys(&ComplianceKeyQuery {
                include_inactive: true,
                ..Default::default()
            })
            .unwrap()
            .len(),
        24
    );
}

#[cfg(unix)]
#[test]
fn nonempty_legacy_schema_requires_a_matching_signed_checkpoint() {
    let dir = secure_tempdir();
    let path = dir.path().join("legacy.db");
    create_legacy_db(&path, false);

    let refused = open_error(path.to_str().unwrap());
    assert!(matches!(refused, RegistryError::MigrationRequired(_)));

    let (root, size) = legacy_root();
    let wrong_checkpoint = Checkpoint {
        origin: ORIGIN.into(),
        size,
        root: [0x99; 32],
    }
    .sign(&config().signing_key);
    assert!(matches!(
        Registry::open_with_legacy_checkpoint(path.to_str().unwrap(), config(), &wrong_checkpoint,),
        Err(RegistryError::MigrationRequired(_))
    ));
    let untouched = Connection::open(&path).unwrap();
    assert_eq!(
        untouched
            .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        0
    );
    assert_eq!(
        untouched
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(untouched);

    let checkpoint = Checkpoint {
        origin: ORIGIN.into(),
        size,
        root,
    }
    .sign(&config().signing_key);
    let migrated =
        Registry::open_with_legacy_checkpoint(path.to_str().unwrap(), config(), &checkpoint)
            .unwrap();
    assert_eq!(migrated.size().unwrap(), 2);
    assert_eq!(migrated.head().unwrap().root, root);
    let migrated_entries = migrated.dump().unwrap();
    assert_eq!(migrated_entries[0].version(), 0);
    let (legacy_handle, legacy_key, legacy_subject) = migrated_entries[0].handle_binding().unwrap();
    assert_eq!(legacy_handle, "/gh/alice");
    assert_eq!(legacy_key, "11".repeat(32));
    assert_eq!(legacy_subject, "gh:alice");
    assert!(
        Handle::parse("/gh/alice").is_err(),
        "historical auditability must not create a public resolution alias"
    );
    drop(migrated);

    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 9);
    let (migration_minute, migration_admissions): (i64, i64) = connection
        .query_row(
            "SELECT window_minute, admissions FROM global_binding_admission WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(migration_minute > 0);
    assert_eq!(migration_admissions, 1_000_000);
    let legacy_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM legacy_entries_v0", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(legacy_rows, 2, "the authenticated source remains auditable");
}

#[cfg(unix)]
#[test]
fn schema_v4_upgrade_discards_unbound_ephemeral_challenges() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry-v4.db");
    drop(Registry::open(path.to_str().unwrap(), config()).unwrap());

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE global_binding_admission;
             DROP TABLE directory_mutations;
             CREATE TABLE directory_mutations (
                 endpoint TEXT PRIMARY KEY,
                 loft_pubkey TEXT NOT NULL,
                 mutation_sequence INTEGER NOT NULL CHECK (mutation_sequence >= 0),
                 entry_seq INTEGER NOT NULL UNIQUE REFERENCES entries(seq),
                 mutation_kind TEXT NOT NULL CHECK (mutation_kind IN ('directory_add', 'directory_remove'))
             );
             DROP INDEX identity_challenges_expiry;
             DROP TABLE identity_challenges;
             CREATE TABLE identity_challenges (
                 challenge_hash BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
                 provider TEXT NOT NULL CHECK (provider IN ('github', 'google')),
                 pkce_challenge TEXT,
                 expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0),
                 consumed_at_ms INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
                 CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                     OR (provider = 'google' AND pkce_challenge IS NULL))
             );
             CREATE INDEX identity_challenges_expiry
                 ON identity_challenges (expires_at_ms, consumed_at_ms);
             DROP INDEX current_bindings_by_subject;
             DROP TABLE current_bindings;
             CREATE TABLE current_bindings (
                 handle TEXT PRIMARY KEY,
                 pubkey TEXT NOT NULL,
                 subject TEXT NOT NULL UNIQUE,
                 seq INTEGER NOT NULL UNIQUE REFERENCES entries(seq)
             );
             DELETE FROM schema_migrations WHERE version >= 5;
             INSERT INTO schema_migrations
                 (version, applied_at_ms, source_schema, authorization_checkpoint)
                 VALUES (4, 1, 'schema-v3', NULL);
             PRAGMA user_version = 4;",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO identity_challenges
             (challenge_hash, provider, pkce_challenge, expires_at_ms, consumed_at_ms)
             VALUES (?1, 'github', ?2, ?3, NULL)",
            params![vec![7u8; 32], "a".repeat(43), i64::MAX],
        )
        .unwrap();
    drop(connection);

    drop(Registry::open(path.to_str().unwrap(), config()).unwrap());
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 9);
    let remaining: i64 = connection
        .query_row("SELECT COUNT(*) FROM identity_challenges", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        remaining, 0,
        "unbound v4 challenges must not survive migration"
    );
    let mut statement = connection
        .prepare("PRAGMA table_info(identity_challenges)")
        .unwrap();
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(columns.contains(&"handle".to_owned()));
    assert!(columns.contains(&"pubkey".to_owned()));
}

#[cfg(unix)]
#[test]
fn schema_v5_directory_projection_migrates_to_independent_key_streams() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry-v5.db");
    let first_key = SigningKey::from_bytes(&[51; 32]);
    {
        let registry = Registry::open(path.to_str().unwrap(), config()).unwrap();
        registry
            .append_directory_add(directory_addition(
                &first_key,
                "https://loft.example",
                1,
                10,
            ))
            .unwrap();
    }

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE global_binding_admission;
             ALTER TABLE directory_mutations RENAME TO directory_mutations_v6;
             CREATE TABLE directory_mutations (
                 endpoint TEXT PRIMARY KEY,
                 loft_pubkey TEXT NOT NULL,
                 mutation_sequence INTEGER NOT NULL CHECK (mutation_sequence >= 0),
                 entry_seq INTEGER NOT NULL UNIQUE REFERENCES entries(seq),
                 mutation_kind TEXT NOT NULL CHECK (mutation_kind IN ('directory_add', 'directory_remove'))
             );
             INSERT INTO directory_mutations
                 (endpoint, loft_pubkey, mutation_sequence, entry_seq, mutation_kind)
                 SELECT endpoint, loft_pubkey, mutation_sequence, entry_seq, mutation_kind
                 FROM directory_mutations_v6;
             DROP TABLE directory_mutations_v6;
             DROP INDEX identity_challenges_expiry;
             DROP TABLE identity_challenges;
             CREATE TABLE identity_challenges (
                 challenge_hash BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
                 provider TEXT NOT NULL CHECK (provider IN ('github', 'google')),
                 handle TEXT NOT NULL,
                 pubkey BLOB NOT NULL CHECK (length(pubkey) = 32),
                 pkce_challenge TEXT,
                 expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0),
                 consumed_at_ms INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
                 CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                     OR (provider = 'google' AND pkce_challenge IS NULL))
             );
             CREATE INDEX identity_challenges_expiry
                 ON identity_challenges (expires_at_ms, consumed_at_ms);
             DROP INDEX current_bindings_by_subject;
             DROP TABLE current_bindings;
             CREATE TABLE current_bindings (
                 handle TEXT PRIMARY KEY,
                 pubkey TEXT NOT NULL,
                 subject TEXT NOT NULL UNIQUE,
                 seq INTEGER NOT NULL UNIQUE REFERENCES entries(seq)
             );
             DELETE FROM schema_migrations WHERE version >= 6;
             INSERT INTO schema_migrations
                 (version, applied_at_ms, source_schema, authorization_checkpoint)
                 VALUES (5, 1, 'schema-v4', NULL);
             PRAGMA user_version = 5;",
        )
        .unwrap();
    drop(connection);

    let registry = Registry::open(path.to_str().unwrap(), config()).unwrap();
    let second_key = SigningKey::from_bytes(&[52; 32]);
    let second = registry
        .append_directory_add(directory_addition(
            &second_key,
            "https://loft.example",
            1,
            10,
        ))
        .unwrap();
    assert!(second.appended);
    assert_eq!(registry.audit_storage().unwrap(), 2);
    drop(registry);

    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 9);
    let claim_streams: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM directory_mutations WHERE endpoint = ?1",
            params!["https://loft.example"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claim_streams, 2);
}

#[cfg(unix)]
#[test]
fn schema_v6_upgrade_adds_durable_global_binding_admission() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry-v6.db");
    drop(Registry::open(path.to_str().unwrap(), config()).unwrap());

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE global_binding_admission;
             DROP INDEX identity_challenges_expiry;
             DROP TABLE identity_challenges;
             CREATE TABLE identity_challenges (
                 challenge_hash BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
                 provider TEXT NOT NULL CHECK (provider IN ('github', 'google')),
                 handle TEXT NOT NULL,
                 pubkey BLOB NOT NULL CHECK (length(pubkey) = 32),
                 pkce_challenge TEXT,
                 expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0),
                 consumed_at_ms INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
                 CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                     OR (provider = 'google' AND pkce_challenge IS NULL))
             );
             CREATE INDEX identity_challenges_expiry
                 ON identity_challenges (expires_at_ms, consumed_at_ms);
             DROP INDEX current_bindings_by_subject;
             DROP TABLE current_bindings;
             CREATE TABLE current_bindings (
                 handle TEXT PRIMARY KEY,
                 pubkey TEXT NOT NULL,
                 subject TEXT NOT NULL UNIQUE,
                 seq INTEGER NOT NULL UNIQUE REFERENCES entries(seq)
             );
             DELETE FROM schema_migrations WHERE version >= 7;
             INSERT INTO schema_migrations
                 (version, applied_at_ms, source_schema, authorization_checkpoint)
                 VALUES (6, 1, 'schema-v5', NULL);
             PRAGMA user_version = 6;",
        )
        .unwrap();
    drop(connection);

    drop(Registry::open(path.to_str().unwrap(), config()).unwrap());
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 9);
    let state: (i64, i64) = connection
        .query_row(
            "SELECT window_minute, admissions FROM global_binding_admission WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(state.0 > 0);
    assert_eq!(state.1, 1_000_000);
}

#[cfg(unix)]
#[test]
fn unknown_legacy_kind_fails_closed_before_any_migration() {
    let dir = secure_tempdir();
    let path = dir.path().join("legacy.db");
    create_legacy_db(&path, true);
    let error = open_error(path.to_str().unwrap());
    assert!(matches!(error, RegistryError::MigrationRequired(_)));
    let connection = Connection::open(path).unwrap();
    let still_old: i64 = connection
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(still_old, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn tampered_persisted_state_is_refused_on_restart() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry.db");
    {
        let registry = with_test_trace(
            Registry::open(path.to_str().unwrap(), config())
                .unwrap()
                .with_provider(Box::new(MockProvider)),
        );
        add(&registry, 1, "alice").await;
    }
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE registry_state SET root = zeroblob(32)", [])
        .unwrap();
    let error = open_error(path.to_str().unwrap());
    assert!(matches!(error, RegistryError::CorruptStorage(_)));
}

#[cfg(unix)]
#[test]
fn missing_directory_projection_is_refused_before_replay_checks_can_be_bypassed() {
    let dir = secure_tempdir();
    let path = dir.path().join("registry.db");
    {
        let registry = Registry::open(path.to_str().unwrap(), config()).unwrap();
        let key = SigningKey::from_bytes(&[31; 32]);
        registry
            .append_directory_add(directory_addition(&key, "https://loft.example", 1, 10))
            .unwrap();
    }
    Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM directory_mutations", [])
        .unwrap();
    let error = open_error(path.to_str().unwrap());
    assert!(matches!(error, RegistryError::CorruptStorage(_)));
}

#[cfg(feature = "test-utilities")]
#[tokio::test]
async fn http_reads_have_etags_conditional_responses_ranges_and_compliance_proofs() {
    let registry = Arc::new(registry());
    add(&registry, 1, "alice").await;
    let start = now_ms() / (24 * 60 * 60 * 1_000) * (24 * 60 * 60 * 1_000);
    let key_id = ComplianceKeyId::new(
        CompliancePurpose::NetworkTrace,
        Jurisdiction::Tr,
        [9; 32],
        start,
        1,
    );
    registry
        .publish_compliance_key(ComplianceKeyPublish {
            key_id,
            public_key: "33".repeat(32),
            not_before_ms: start,
            not_after_ms: start + 24 * 60 * 60 * 1_000,
            status: ComplianceKeyStatus::Active,
        })
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(pigeonpost_registry::serve_loopback_test(
        listener,
        Arc::clone(&registry),
        test_http_config(),
        stopped,
    ));
    let http = reqwest::Client::new();

    let legacy_alias = http
        .get(format!("{base}/v1/resolve/gh/alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        legacy_alias.status(),
        400,
        "legacy /gh is not a resolve alias"
    );

    let first = http
        .get(format!("{base}/v1/resolve/github/alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    assert_eq!(
        first.headers()["cache-control"],
        "public, max-age=60, must-revalidate"
    );
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    let resolved: serde_json::Value = first.json().await.unwrap();
    assert!(resolved["inclusion_proof"]["checkpoint"]
        .as_str()
        .unwrap()
        .contains(ORIGIN));
    let unchanged = http
        .get(format!("{base}/v1/resolve/github/alice"))
        .header("if-none-match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(unchanged.status(), 304);
    assert!(unchanged.bytes().await.unwrap().is_empty());

    let entries: serde_json::Value = http
        .get(format!("{base}/v1/log/entries?from=0&to=2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(entries["entries"].as_array().unwrap().len(), 2);
    assert_eq!(entries["entries"][1]["type"], "compliance_key_publish");

    let key_hex = pigeonpost_registry::registry::hex(&key_id.encode().unwrap());
    assert_eq!(key_hex.len(), COMPLIANCE_KEY_ID_LEN * 2);
    let keys: serde_json::Value = http
        .get(format!("{base}/v1/compliance-keys/{key_hex}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(keys["keys"].as_array().unwrap().len(), 1);
    assert_eq!(keys["keys"][0]["key_id_hex"], key_hex);
    assert!(keys["keys"][0]["inclusion_path"].is_array());
    Checkpoint::verify(
        keys["checkpoint"].as_str().unwrap(),
        &registry.verifying_key(),
    )
    .unwrap();

    let dump = http
        .get(format!("{base}/v1/log/dump"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        dump.headers()["cache-control"],
        "public, max-age=300, must-revalidate"
    );
    let body = dump.text().await.unwrap();
    assert_eq!(body.lines().count(), 2);
    for line in body.lines() {
        serde_json::from_str::<LogEntry>(line).unwrap();
    }
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[cfg(feature = "test-utilities")]
#[tokio::test]
async fn http_directory_routes_return_the_exact_proven_leaf_and_are_idempotent() {
    let registry = Arc::new(registry());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(pigeonpost_registry::serve_loopback_test(
        listener,
        Arc::clone(&registry),
        test_http_config(),
        stopped,
    ));
    let key = SigningKey::from_bytes(&[51; 32]);
    let addition = directory_addition(&key, "https://loft.example", 1, 10);
    let body = serde_json::to_vec(&addition).unwrap();
    let headers = directory_auth_headers(DirectoryMutationOperation::Add, &body);
    let http = reqwest::Client::new();

    let first: serde_json::Value = http
        .post(format!("{base}/v1/directory/add"))
        .header(headers[0].0, &headers[0].1)
        .header(headers[1].0, &headers[1].1)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["log_index"], 0);
    assert_eq!(first["appended"], true);
    assert_eq!(
        serde_json::from_value::<LogEntry>(first["entry"].clone()).unwrap(),
        registry.entry(0).unwrap()
    );
    assert!(first["inclusion_proof"]["checkpoint"].is_string());

    let retry: serde_json::Value = http
        .post(format!("{base}/v1/directory/add"))
        .header(headers[0].0, &headers[0].1)
        .header(headers[1].0, &headers[1].1)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["appended"], false);
    assert_eq!(registry.size().unwrap(), 1);

    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[cfg(feature = "test-utilities")]
#[tokio::test]
async fn http_registration_uses_the_bounded_lane_and_source_rate_limit() {
    use pigeonpost_registry::RegistryLimits;

    let registry = Arc::new(registry());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let limits = RegistryLimits {
        source_bindings_per_minute: 1,
        ..RegistryLimits::default()
    };
    let config = test_http_config().with_limits(limits).unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(pigeonpost_registry::serve_loopback_test(
        listener,
        Arc::clone(&registry),
        config,
        stopped,
    ));

    let identity = Identity::from_seed([91; 32]);
    let handle = Handle::parse("/github/alice").unwrap();
    let (pubkey, signature) = claim(&identity, &handle);
    let body = serde_json::json!({
        "handle": handle.as_path(),
        "pubkey": pigeonpost_registry::registry::hex(&pubkey),
        "signature": pigeonpost_registry::registry::hex(&signature),
        "proof": mock("alice"),
    });
    let http = reqwest::Client::new();
    let registered = http
        .post(format!("{base}/v1/register"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);

    let limited = http
        .post(format!("{base}/v1/register"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), 429);
    assert_eq!(registry.size().unwrap(), 1);
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[cfg(feature = "test-utilities")]
#[tokio::test]
async fn http_account_limit_survives_trusted_proxy_source_rotation() {
    let registry = Arc::new(
        registry()
            .with_registration_limits(RegistrationLimits {
                account_bindings_per_minute: 1,
                max_account_keys: 8,
                ..RegistrationLimits::default()
            })
            .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let config = pigeonpost_registry::RegistryHttpConfig::with_trusted_proxies(vec!["127.0.0.1"
        .parse()
        .unwrap()])
    .unwrap()
    .with_directory_publishers(vec![test_directory_publisher().verifying_key()])
    .unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(pigeonpost_registry::serve_loopback_test(
        listener,
        Arc::clone(&registry),
        config,
        stopped,
    ));

    let alice_identity = Identity::from_seed([92; 32]);
    let alice = Handle::parse("/github/alice").unwrap();
    let (alice_key, alice_signature) = claim(&alice_identity, &alice);
    let alice_body = serde_json::json!({
        "handle": alice.as_path(),
        "pubkey": pigeonpost_registry::registry::hex(&alice_key),
        "signature": pigeonpost_registry::registry::hex(&alice_signature),
        "proof": mock("alice"),
    });
    let http = reqwest::Client::new();
    let first = http
        .post(format!("{base}/v1/register"))
        .header("forwarded", "for=192.0.2.11:4111")
        .json(&alice_body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);

    let rotated_source = http
        .post(format!("{base}/v1/register"))
        .header("forwarded", "for=198.51.100.22:4222")
        .json(&alice_body)
        .send()
        .await
        .unwrap();
    assert_eq!(rotated_source.status(), 429);

    let bob_identity = Identity::from_seed([93; 32]);
    let bob = Handle::parse("/github/bob").unwrap();
    let (bob_key, bob_signature) = claim(&bob_identity, &bob);
    let bob_body = serde_json::json!({
        "handle": bob.as_path(),
        "pubkey": pigeonpost_registry::registry::hex(&bob_key),
        "signature": pigeonpost_registry::registry::hex(&bob_signature),
        "proof": mock("bob"),
    });
    let distinct_subject = http
        .post(format!("{base}/v1/register"))
        .header("forwarded", "for=203.0.113.33:4333")
        .json(&bob_body)
        .send()
        .await
        .unwrap();
    assert_eq!(distinct_subject.status(), 200);
    assert_eq!(registry.size().unwrap(), 2);

    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
fn create_legacy_db(path: &std::path::Path, include_unknown: bool) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/v0_1_0_registry.sql"))
        .unwrap();
    if include_unknown {
        connection
            .execute(
                "INSERT INTO entries (idx, kind, handle, pubkey, subject, timestamp)
                 VALUES (2, 'future_kind', '/gh/mallory', ?1, 'gh:mallory', 1786105723)",
                params!["33".repeat(32)],
            )
            .unwrap();
    }
    drop(connection);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}

#[cfg(unix)]
fn legacy_root() -> ([u8; 32], u64) {
    let mut log = MerkleLog::new();
    log.append(&legacy_leaf(
        0,
        1,
        "/gh/alice",
        &"11".repeat(32),
        "gh:alice",
        1_786_105_721,
    ));
    log.append(&legacy_leaf(
        1,
        2,
        "/gh/alice",
        &"22".repeat(32),
        "gh:alice",
        1_786_105_722,
    ));
    (log.root(), log.size())
}

#[cfg(unix)]
fn legacy_leaf(
    seq: u64,
    tag: u8,
    handle: &str,
    pubkey: &str,
    subject: &str,
    timestamp: u64,
) -> Vec<u8> {
    let mut bytes = b"pigeonpost/log-entry/v1".to_vec();
    bytes.push(tag);
    bytes.extend_from_slice(&seq.to_le_bytes());
    for field in [handle.as_bytes(), pubkey.as_bytes(), subject.as_bytes()] {
        bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
        bytes.extend_from_slice(field);
    }
    bytes.extend_from_slice(&timestamp.to_le_bytes());
    bytes
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(unix)]
fn open_error(path: &str) -> RegistryError {
    match Registry::open(path, config()) {
        Ok(_) => panic!("registry unexpectedly opened"),
        Err(error) => error,
    }
}
