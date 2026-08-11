#![cfg(feature = "server")]

//! M1 acceptance: two agents exchange an encrypted message through a real loft, with the
//! recipient **offline at send time**.
//!
//! These drive a real Axum server over a real TCP socket with the real wire format — no mocks,
//! no in-process shortcuts. If any of this passes while the product is broken, the test is wrong.

use std::sync::Arc;

use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
use pigeonpost_core::{
    envelope, keys::SuccessorCommitment, AgentRecord, AttributionRequirement, FetchAuth, Identity,
    RecipientPolicy, RotationRecord, Token,
};
use pigeonpost_loft::wire::FetchRequest;
use pigeonpost_loft::{
    client::ClientError, AttributionKeyResolver, AttributionResolutionError, Loft, LoftClient,
    LoftConfig, LoftStore, ResolvedAttributionKey, SqliteStore, TraceInput, TraceSink,
    TraceSinkError,
};
use pigeonpost_registry::ComplianceKeyStatus;

/// Message timestamps are arbitrary — they are jittered and never checked against a clock.
const NOW: u64 = 1_786_105_721;

/// Fetch proofs *are* clock-bound (`FetchAuth`), so they must use the real time the server sees.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Boot a loft on an ephemeral port. Returns a client pointed at it, plus its pubkey.
async fn spawn_loft(config: LoftConfig) -> (LoftClient, [u8; 32]) {
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    spawn_instance(Loft::new(config, store).unwrap()).await
}

async fn spawn_instance(mut loft: Loft) -> (LoftClient, [u8; 32]) {
    let pubkey = loft.config.pubkey;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    loft.config.origin = format!("http://{addr}");
    let loft = Arc::new(loft);
    tokio::spawn(async move {
        pigeonpost_loft::serve(listener, loft, std::future::pending::<()>())
            .await
            .unwrap();
    });

    (LoftClient::new(format!("http://{addr}")).unwrap(), pubkey)
}

struct FixedResolver {
    id: ComplianceKeyId,
    key: ResolvedAttributionKey,
}

impl AttributionKeyResolver for FixedResolver {
    fn resolve(
        &self,
        key_id: &ComplianceKeyId,
    ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError> {
        Ok((*key_id == self.id).then_some(self.key))
    }
}

struct FailingTrace;

impl TraceSink for FailingTrace {
    fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
        Ok(())
    }

    fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
        Err(TraceSinkError::Unavailable)
    }
}

struct SlowTrace;

impl TraceSink for SlowTrace {
    fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
        Ok(())
    }

    fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
        std::thread::sleep(std::time::Duration::from_millis(100));
        Ok(())
    }
}

fn loft_config() -> LoftConfig {
    LoftConfig::new([0xAB; 32], "http://127.0.0.1:1")
}

#[tokio::test]
async fn a_message_waits_for_an_agent_that_was_never_online() {
    let (client, loft_key) = spawn_loft(loft_config()).await;

    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    // Bob has never contacted this loft. He is not connected, and never has been.
    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "the build is green", NOW).unwrap();
    let published = client.publish(&wrap, None).await.unwrap();
    assert!(published.stored);

    // Time passes. Bob wakes up.
    let drained = client
        .fetch(&bob, &loft_key, 0, now_secs(), None)
        .await
        .unwrap();

    assert_eq!(drained.events.len(), 1);
    let (sender, body) = envelope::open(&bob, &drained.events[0]).unwrap();
    assert_eq!(sender, alice.verifying_key());
    assert_eq!(body.as_str(), "the build is green");
}

#[tokio::test]
async fn the_loft_cannot_read_what_it_stores() {
    let (client, loft_key) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let secret = "the passphrase is hunter2";
    let wrap = envelope::wrap(&alice, &bob.verifying_key(), secret, NOW).unwrap();
    client.publish(&wrap, None).await.unwrap();

    // Everything the loft holds, as it holds it.
    let stored = client
        .fetch(&bob, &loft_key, 0, now_secs(), None)
        .await
        .unwrap();
    let raw = serde_json::to_string(&stored.events[0]).unwrap();

    assert!(!raw.contains(secret), "plaintext must never be stored");
    assert!(
        !raw.contains(&hex(alice.verifying_key().as_bytes())),
        "the sender's identity must not be visible to the operator"
    );
}

#[tokio::test]
async fn nobody_else_can_drain_your_mailbox() {
    let (client, loft_key) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let eve = Identity::from_seed([3; 32]);

    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "private", NOW).unwrap();
    client.publish(&wrap, None).await.unwrap();

    // Eve signs a perfectly valid proof — for her own mailbox, which is empty.
    let eve_fetch = client
        .fetch(&eve, &loft_key, 0, now_secs(), None)
        .await
        .unwrap();
    assert!(eve_fetch.events.is_empty());

    // And a proof for the wrong loft is refused outright.
    let wrong_loft = client.fetch(&bob, &[0xFF; 32], 0, now_secs(), None).await;
    assert!(matches!(
        wrong_loft,
        Err(ClientError::Refused { status: 401, .. })
    ));
}

#[tokio::test]
async fn a_harvester_cannot_bulk_fetch_with_forged_victim_proofs() {
    let (client, loft_key) = spawn_loft(loft_config().with_rate_limit(10_000)).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let eve = Identity::from_seed([3; 32]);

    for sequence in 0..16 {
        let wrap = envelope::wrap(
            &alice,
            &bob.verifying_key(),
            &format!("private batch {sequence}"),
            NOW,
        )
        .unwrap();
        client.publish(&wrap, None).await.unwrap();
    }

    let http = reqwest::Client::new();
    for cursor in 0..32 {
        let mut auth =
            FetchAuth::new(&eve, &loft_key, client.base_url(), now_secs() / 60, cursor).unwrap();
        auth.recipient = bob.verifying_key().to_bytes();
        let response = http
            .post(format!("{}/v1/fetch", client.base_url()))
            .json(&FetchRequest {
                auth,
                limit: Some(500),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    let mut cursor = 0;
    let mut received = 0;
    loop {
        let victim = client
            .fetch(&bob, &loft_key, cursor, now_secs(), Some(500))
            .await
            .unwrap();
        received += victim.events.len();
        if !victim.more {
            break;
        }
        assert!(victim.next_cursor > cursor);
        cursor = victim.next_cursor;
    }
    assert_eq!(received, 16);
}

#[tokio::test]
async fn a_stale_fetch_proof_is_refused() {
    let (client, loft_key) = spawn_loft(loft_config()).await;
    let bob = Identity::from_seed([2; 32]);

    // A proof signed for a time an hour ago: intercepted, replayed too late.
    let result = client
        .fetch(&bob, &loft_key, 0, now_secs() - 3_600, None)
        .await;

    assert!(matches!(
        result,
        Err(ClientError::Refused { status: 401, .. })
    ));
}

#[tokio::test]
async fn draining_advances_a_cursor_and_does_not_repeat() {
    let (client, loft_key) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    for i in 0..5 {
        let wrap =
            envelope::wrap(&alice, &bob.verifying_key(), &format!("message {i}"), NOW).unwrap();
        client.publish(&wrap, None).await.unwrap();
    }

    let first = client
        .fetch(&bob, &loft_key, 0, now_secs(), Some(2))
        .await
        .unwrap();
    assert_eq!(first.events.len(), 2);
    assert!(first.more, "the client should know to drain again");

    let second = client
        .fetch(&bob, &loft_key, first.next_cursor, now_secs(), Some(10))
        .await
        .unwrap();
    assert_eq!(second.events.len(), 3);
    assert!(!second.more);

    // Nothing new since.
    let third = client
        .fetch(&bob, &loft_key, second.next_cursor, now_secs(), None)
        .await
        .unwrap();
    assert!(third.events.is_empty());
    assert_eq!(third.next_cursor, second.next_cursor);
}

#[tokio::test]
async fn the_same_message_from_several_lofts_deduplicates() {
    // Senders publish to 2-3 lofts; the id is what lets a client drop the copies.
    let (client, _) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "once", NOW).unwrap();
    let first = client.publish(&wrap, None).await.unwrap();
    let again = client.publish(&wrap, None).await.unwrap();

    assert_eq!(first.id, again.id);
    assert!(first.stored);
    assert!(!again.stored, "a retry is success, not an error");
}

#[tokio::test]
async fn proof_of_work_is_enforced_at_the_recipients_floor() {
    let (client, loft_key) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    // Bob demands work from everyone. A loft cannot see senders, so the floor is flat.
    let policy = RecipientPolicy::new(&bob, 8, false, vec![], 1);
    client.set_policy(&policy).await.unwrap();

    let unstamped = envelope::wrap(&alice, &bob.verifying_key(), "unstamped", NOW).unwrap();
    let refused = client.publish(&unstamped, None).await;
    assert!(matches!(
        refused,
        Err(ClientError::Refused { status: 403, .. })
    ));

    let mut stamped = envelope::wrap(&alice, &bob.verifying_key(), "stamped", NOW).unwrap();
    stamped.stamp(8, 5_000_000).unwrap();
    assert!(client.publish(&stamped, None).await.unwrap().stored);

    let drained = client
        .fetch(&bob, &loft_key, 0, now_secs(), None)
        .await
        .unwrap();
    assert_eq!(drained.events.len(), 1);
}

#[tokio::test]
async fn a_policy_replay_cannot_reopen_a_revoked_token() {
    let (client, _) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let readme = Token::mint(&[7; 32], "readme");
    let presented = readme.presentation(&[0xAB; 32], client.base_url()).unwrap();

    // Bob publishes an open inbox gated on the readme token.
    let open = RecipientPolicy::new(&bob, 0, true, vec![*presented.as_bytes()], 1);
    client.set_policy(&open).await.unwrap();

    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
    assert!(client
        .publish(&wrap, Some(hex(presented.as_bytes())))
        .await
        .is_ok());

    // The token gets harvested, so Bob revokes it.
    let revoked = RecipientPolicy::new(&bob, 0, true, vec![], 2);
    client.set_policy(&revoked).await.unwrap();

    let after = envelope::wrap(&alice, &bob.verifying_key(), "spam", NOW).unwrap();
    assert!(client
        .publish(&after, Some(hex(presented.as_bytes())))
        .await
        .is_err());

    // An attacker replays the earlier, still validly-signed policy to turn it back on.
    let replayed = client.set_policy(&open).await;
    assert!(
        matches!(replayed, Err(ClientError::Refused { status: 409, .. })),
        "replaying an old policy must not undo a revocation"
    );
}

#[tokio::test]
async fn a_token_does_not_replay_to_another_loft() {
    let (loft_a, _) = spawn_loft(LoftConfig::new([0xBB; 32], "http://127.0.0.1:1")).await;
    let (loft_b, _) = spawn_loft(LoftConfig::new([0xBB; 32], "http://127.0.0.1:1")).await;

    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let token = Token::mint(&[7; 32], "readme");

    // Bob registers the token at both lofts — bound to each loft's key and exact origin.
    loft_a
        .set_policy(&RecipientPolicy::new(
            &bob,
            0,
            true,
            vec![*token
                .presentation(&[0xBB; 32], loft_a.base_url())
                .unwrap()
                .as_bytes()],
            1,
        ))
        .await
        .unwrap();
    loft_b
        .set_policy(&RecipientPolicy::new(
            &bob,
            0,
            true,
            vec![*token
                .presentation(&[0xBB; 32], loft_b.base_url())
                .unwrap()
                .as_bytes()],
            1,
        ))
        .await
        .unwrap();

    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "hi", NOW).unwrap();
    let at_a = hex(token
        .presentation(&[0xBB; 32], loft_a.base_url())
        .unwrap()
        .as_bytes());

    assert!(loft_a.publish(&wrap, Some(at_a.clone())).await.is_ok());
    assert!(
        loft_b.publish(&wrap, Some(at_a)).await.is_err(),
        "an endpoint claiming another loft's key must not obtain a replayable presentation"
    );
}

#[tokio::test]
async fn a_fetch_credential_does_not_replay_across_origins_claiming_the_same_key() {
    let key = [0xCC; 32];
    let (loft_a, _) = spawn_loft(LoftConfig::new(key, "http://127.0.0.1:1")).await;
    let (loft_b, _) = spawn_loft(LoftConfig::new(key, "http://127.0.0.1:1")).await;
    let bob = Identity::from_seed([2; 32]);
    let auth = FetchAuth::new(&bob, &key, loft_a.base_url(), now_secs() / 60, 0).unwrap();

    let response = reqwest::Client::new()
        .post(format!("{}/v1/fetch", loft_b.base_url()))
        .json(&FetchRequest { auth, limit: None })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oversized_mail_is_refused() {
    let (_client, _) = spawn_loft(loft_config().with_capacity_bytes(1024 * 1024)).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let huge = "x".repeat(envelope::MAX_PLAINTEXT + 1);
    assert!(envelope::wrap(&alice, &bob.verifying_key(), &huge, NOW).is_err());
}

#[tokio::test]
async fn maximum_plaintext_fits_the_default_wire_limits_and_one_byte_over_is_refused() {
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    // NUL forces serde_json's six-byte `\u0000` escape for every plaintext byte. This is the
    // protocol's largest legal JSON expansion, not merely a friendly ASCII payload.
    let body = "\0".repeat(envelope::MAX_PLAINTEXT);
    let wrap = envelope::wrap(&alice, &bob.verifying_key(), &body, NOW).unwrap();
    let event_bytes = serde_json::to_vec(&wrap).unwrap().len();
    let request_bytes = serde_json::to_vec(&pigeonpost_loft::wire::PublishRequest {
        wrap: wrap.clone(),
        token: None,
    })
    .unwrap()
    .len();
    let defaults = loft_config();
    assert!(event_bytes <= defaults.max_event_bytes);
    assert!(request_bytes <= defaults.max_request_bytes);

    let (client, loft_key) = spawn_loft(defaults).await;
    assert!(client.publish(&wrap, None).await.unwrap().stored);
    let drained = client
        .fetch(&bob, &loft_key, 0, now_secs(), None)
        .await
        .unwrap();
    let (_, opened) = envelope::open(&bob, &drained.events[0]).unwrap();
    assert_eq!(opened.as_str(), body);

    let mut undersized = loft_config();
    undersized.max_event_bytes = event_bytes - 1;
    let (client, _) = spawn_loft(undersized).await;
    assert!(matches!(
        client.publish(&wrap, None).await,
        Err(ClientError::Refused { status: 413, .. })
    ));
}

#[tokio::test]
async fn a_full_loft_says_no_rather_than_absorbing_the_cost() {
    // Capacity is a budget, not free disk. Refusing is the honest failure.
    let (client, _) = spawn_loft(loft_config().with_capacity_bytes(2_000)).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let mut refusals = 0;
    for i in 0..20 {
        let wrap =
            envelope::wrap(&alice, &bob.verifying_key(), &format!("message {i}"), NOW).unwrap();
        if let Err(ClientError::Refused { status: 507, .. }) = client.publish(&wrap, None).await {
            refusals += 1;
        }
    }
    assert!(refusals > 0, "a full loft must refuse, not grow");
}

#[tokio::test]
async fn rate_limiting_bounds_one_mailbox() {
    let (client, _) = spawn_loft(loft_config().with_rate_limit(3)).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let mut limited = 0;
    for i in 0..10 {
        let wrap = envelope::wrap(&alice, &bob.verifying_key(), &format!("m{i}"), NOW).unwrap();
        if let Err(ClientError::Refused { status: 429, .. }) = client.publish(&wrap, None).await {
            limited += 1;
        }
    }
    assert_eq!(limited, 7);
}

#[tokio::test]
async fn malformed_outer_signature_is_rejected_before_charging_the_recipient() {
    let (client, _) = spawn_loft(loft_config().with_rate_limit(1)).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    let good = envelope::wrap(&alice, &bob.verifying_key(), "valid", NOW).unwrap();
    let mut forged = good.clone();
    forged.signature[0] ^= 1;
    assert!(matches!(
        client.publish(&forged, None).await,
        Err(ClientError::Refused { status: 401, .. })
    ));
    assert!(client.publish(&good, None).await.unwrap().stored);
}

#[tokio::test]
async fn only_v3_writes_are_admitted() {
    let (client, _) = spawn_loft(loft_config()).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let mut legacy_shaped = envelope::wrap(&alice, &bob.verifying_key(), "old", NOW).unwrap();
    legacy_shaped.version = envelope::LEGACY_ENVELOPE_VERSION;

    assert!(matches!(
        client.publish(&legacy_shaped, None).await,
        Err(ClientError::Refused { status: 400, .. })
    ));
    assert_eq!(client.info().await.unwrap().event_count, 0);
}

#[tokio::test]
async fn attribution_required_is_enforced_with_a_live_registry_key() {
    let compliance_key = [0xC1; 32];
    let key_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0xA1; 32],
        1_785_542_400_000,
        1,
    );
    let resolver: Arc<dyn AttributionKeyResolver> = Arc::new(FixedResolver {
        id: key_id,
        key: ResolvedAttributionKey {
            public_key: compliance_key,
            not_before_ms: 1_785_542_400_000,
            not_after_ms: 1_788_220_800_000,
            status: ComplianceKeyStatus::Active,
        },
    });
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    let loft = Loft::new(loft_config(), store)
        .unwrap()
        .with_attribution_resolver(resolver);
    let (client, _) = spawn_instance(loft).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    client
        .set_policy(&RecipientPolicy::with_attribution_requirement(
            &bob,
            0,
            false,
            vec![],
            1,
            Some(AttributionRequirement::new(Jurisdiction::Test, [0xA1; 32])),
        ))
        .await
        .unwrap();

    let absent = envelope::wrap(&alice, &bob.verifying_key(), "absent", NOW).unwrap();
    assert!(client.publish(&absent, None).await.is_err());
    let attributed = envelope::wrap_attributed(
        &alice,
        &bob.verifying_key(),
        "present",
        NOW,
        &compliance_key,
        &key_id,
    )
    .unwrap();
    let mut malformed = attributed.clone();
    let envelope::AttributionBlock::V3(block) = malformed.attribution.as_mut().unwrap() else {
        panic!("v3 writer must emit a v3 attribution block");
    };
    block.ciphertext[0] ^= 1;
    assert!(client.publish(&malformed, None).await.is_err());
    assert!(client.publish(&attributed, None).await.unwrap().stored);
}

#[tokio::test]
async fn rotating_recipient_keys_cannot_bypass_shared_source_budget() {
    let mut config = loft_config().with_rate_limit(100);
    config.global_requests_per_minute = 100;
    config.source_requests_per_minute = 3;
    config.source_bytes_per_minute = pigeonpost_loft::MAX_RATE_BYTES_PER_MINUTE;
    let (client, _) = spawn_loft(config).await;
    let alice = Identity::from_seed([1; 32]);

    for seed in 2..=4 {
        let recipient = Identity::from_seed([seed; 32]);
        let wrap = envelope::wrap(&alice, &recipient.verifying_key(), "mail", NOW).unwrap();
        assert!(client.publish(&wrap, None).await.is_ok());
    }
    let recipient = Identity::from_seed([5; 32]);
    let wrap = envelope::wrap(&alice, &recipient.verifying_key(), "blocked", NOW).unwrap();
    assert!(matches!(
        client.publish(&wrap, None).await,
        Err(ClientError::Refused { status: 429, .. })
    ));
}

#[tokio::test]
async fn trusted_proxy_sources_get_independent_admission_buckets() {
    let mut config =
        loft_config().with_trusted_proxies(["127.0.0.1".parse::<std::net::IpAddr>().unwrap()]);
    config.global_requests_per_minute = 100;
    config.global_bytes_per_minute = pigeonpost_loft::MAX_RATE_BYTES_PER_MINUTE;
    config.source_requests_per_minute = 1;
    config.source_bytes_per_minute = pigeonpost_loft::MAX_RATE_BYTES_PER_MINUTE;
    let (client, _) = spawn_loft(config).await;
    let http = reqwest::Client::new();
    let endpoint = format!("{}/v1/publish", client.base_url());

    let send = |source: &'static str| {
        http.post(endpoint.clone())
            .header("Forwarded", source)
            .body("{}")
            .send()
    };
    let first = send("for=\"192.0.2.10:4010\"").await.unwrap();
    assert_ne!(first.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    let repeated = send("for=\"192.0.2.10:4011\"").await.unwrap();
    assert_eq!(repeated.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    let independent = send("for=\"192.0.2.11:4012\"").await.unwrap();
    assert_ne!(
        independent.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "the proxy's transport address must not collapse all clients into one source bucket"
    );

    let missing_source = http.post(endpoint).body("{}").send().await.unwrap();
    assert_eq!(
        missing_source.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "a trusted edge that omits its required source metadata must fail closed"
    );
}

#[tokio::test]
async fn compressed_and_oversized_requests_are_rejected_before_json_work() {
    let (client, _) = spawn_loft(loft_config()).await;
    let http = reqwest::Client::new();
    let compressed = http
        .post(format!("{}/v1/publish", client.base_url()))
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .body("not actually compressed")
        .send()
        .await
        .unwrap();
    assert_eq!(
        compressed.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let oversized = http
        .post(format!("{}/v1/publish", client.base_url()))
        .body("x".repeat(loft_config().max_request_bytes + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(client.info().await.unwrap().event_count, 0);
}

#[tokio::test]
async fn trace_failure_fails_closed_before_storage() {
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    let loft = Loft::new(loft_config(), store)
        .unwrap()
        .with_trace_sink(Arc::new(FailingTrace));
    let (client, _) = spawn_instance(loft).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "not stored", NOW).unwrap();
    assert!(matches!(
        client.publish(&wrap, None).await,
        Err(ClientError::Refused { status: 503, .. })
    ));
    assert_eq!(client.info().await.unwrap().event_count, 0);
}

#[tokio::test]
async fn trace_handoff_has_a_short_fail_closed_deadline() {
    let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
    let mut config = loft_config();
    config.trace_timeout_ms = 10;
    let loft = Loft::new(config, store)
        .unwrap()
        .with_trace_sink(Arc::new(SlowTrace));
    let (client, _) = spawn_instance(loft).await;
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "not stored", NOW).unwrap();

    assert!(matches!(
        client.publish(&wrap, None).await,
        Err(ClientError::Refused { status: 503, .. })
    ));
    assert_eq!(client.info().await.unwrap().event_count, 0);
}

#[tokio::test]
async fn agent_records_round_trip_and_resist_substitution() {
    let (client, _) = spawn_loft(loft_config()).await;
    let agent = Identity::from_seed([4; 32]);
    let other = Identity::from_seed([5; 32]);
    let commitment = SuccessorCommitment::for_key(&Identity::from_seed([6; 32]).verifying_key());

    let record = AgentRecord::new(&agent, &commitment, 1, vec!["https://loft.example".into()]);
    client
        .put_agent_record(&agent.address(), &record)
        .await
        .unwrap();

    let fetched = client.agent_record(&agent.address()).await.unwrap();
    assert_eq!(fetched.pubkey, agent.verifying_key().to_bytes());
    assert_eq!(fetched.lofts, vec!["https://loft.example".to_string()]);

    // A valid record for a different agent cannot be planted at this address.
    let foreign = AgentRecord::new(&other, &commitment, 1, vec![]);
    assert!(client
        .put_agent_record(&agent.address(), &foreign)
        .await
        .is_err());
}

#[tokio::test]
async fn rotation_records_round_trip_and_resist_chain_poisoning() {
    let (client, _) = spawn_loft(loft_config()).await;
    let outgoing = Identity::from_seed([0x41; 32]);
    let incoming = Identity::from_seed([0x42; 32]);
    let next = Identity::from_seed([0x43; 32]);
    let attacker = Identity::from_seed([0x44; 32]);
    let address = outgoing.address();
    let successor = SuccessorCommitment::for_key(&incoming.verifying_key());
    let next_successor = SuccessorCommitment::for_key(&next.verifying_key());
    let activated_at = now_secs();

    let source = AgentRecord::new(&outgoing, &successor, 7, vec![]);
    client.put_agent_record(&address, &source).await.unwrap();
    let rotation =
        RotationRecord::new(&outgoing, &incoming, &next_successor, 8, activated_at).unwrap();
    client
        .put_rotation_record(&address, &rotation)
        .await
        .unwrap();
    // An exact retry is idempotent.
    client
        .put_rotation_record(&address, &rotation)
        .await
        .unwrap();

    let fetched = client.rotation_record(&address).await.unwrap();
    assert_eq!(fetched, rotation);
    assert_eq!(fetched.target_address().unwrap(), incoming.address());

    // Even a valid outgoing signature cannot redirect the precommitted hop or choose the next
    // commitment without the incoming key's cooperation.
    let poisoned_next = SuccessorCommitment::for_key(&attacker.verifying_key());
    let poisoned =
        RotationRecord::new(&outgoing, &attacker, &poisoned_next, 8, activated_at).unwrap();
    assert!(client
        .put_rotation_record(&address, &poisoned)
        .await
        .is_err());
}

#[tokio::test]
async fn info_reports_what_the_prober_needs() {
    let (client, _) = spawn_loft(
        loft_config()
            .with_capacity_bytes(100_000)
            .with_retention_days(7),
    )
    .await;

    let info = client.info().await.unwrap();
    assert_eq!(info.software, "pigeonpost-loft");
    assert_eq!(info.capacity_bytes, 100_000);
    assert_eq!(info.retention_days, 7);
    assert!(info.accepting);
    assert_eq!(info.utilization, 0.0);

    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);
    let wrap = envelope::wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
    client.publish(&wrap, None).await.unwrap();

    let after = client.info().await.unwrap();
    assert!(after.used_bytes > 0);
    assert!(after.utilization > 0.0);
    assert_eq!(after.event_count, 1);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
