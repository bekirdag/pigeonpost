#![cfg(feature = "test-utilities")]

//! GitHub OAuth authorization-code/PKCE/state tests against a real local HTTP stub.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use pigeonpost_compliance_format::{Jurisdiction, TraceCapturePolicy, TraceRetentionPolicy};
use pigeonpost_core::Identity;
use pigeonpost_registry::{
    claim_trace::{ClaimTraceCapacity, ClaimTraceError, ClaimTraceInput, ClaimTraceSink},
    entry::claim_payload,
    identity::{pkce_s256, GithubProvider, ProofPayload},
    Handle, IdentityChallengeProvider, Registry, RegistryConfig, RegistryError,
};
#[cfg(feature = "test-utilities")]
use pigeonpost_registry::{
    identity::GoogleProvider, GITHUB_AUTHORIZATION_ENDPOINT, GOOGLE_AUTHORIZATION_ENDPOINT,
};

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

fn source() -> SocketAddr {
    "192.0.2.11:4242".parse().unwrap()
}

fn with_test_trace(registry: Registry) -> Registry {
    registry.with_claim_trace(Arc::new(TestClaimTraceSink))
}

fn signed_binding(path: &str, seed: u8) -> (Handle, [u8; 32], [u8; 64]) {
    let identity = Identity::from_seed([seed; 32]);
    let handle = Handle::parse(path).unwrap();
    let pubkey = identity.verifying_key().to_bytes();
    let signature = identity
        .sign(&claim_payload(&handle.as_path(), &pubkey))
        .to_bytes();
    (handle, pubkey, signature)
}

#[derive(Default)]
struct StubState {
    token_bodies: Mutex<Vec<String>>,
    user_authorizations: Mutex<Vec<String>>,
}

async fn token(State(state): State<Arc<StubState>>, body: String) -> Json<serde_json::Value> {
    state.token_bodies.lock().unwrap().push(body);
    Json(serde_json::json!({
        "access_token": "provider-access-token",
        "token_type": "bearer",
        "scope": "read:user"
    }))
}

async fn user(State(state): State<Arc<StubState>>, headers: HeaderMap) -> Json<serde_json::Value> {
    state.user_authorizations.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    );
    Json(serde_json::json!({"login": "alice", "id": 12_345_678u64}))
}

async fn oauth_stub() -> (String, Arc<StubState>) {
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .route("/token", post(token))
        .route("/user", get(user))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, state)
}

fn config() -> RegistryConfig {
    RegistryConfig {
        origin: "pigeonpost.dev/registry".into(),
        signing_key: SigningKey::from_bytes(&[42; 32]),
        allow_mock_identities: false,
    }
}

#[cfg(feature = "test-utilities")]
#[tokio::test]
async fn challenge_returns_fixed_public_authorization_metadata_and_never_secrets() {
    let registry = Arc::new(with_test_trace(
        Registry::in_memory(config())
            .unwrap()
            .with_provider(Box::new(GithubProvider::new(
                "github-public-id",
                "github-client-secret-must-not-leak",
            )))
            .with_provider(Box::new(GoogleProvider::new(
                "google-public-id.apps.googleusercontent.com",
            ))),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let http_config = pigeonpost_registry::RegistryHttpConfig::direct()
        .with_directory_publishers(vec![SigningKey::from_bytes(&[89; 32]).verifying_key()])
        .unwrap();
    let task = tokio::spawn(pigeonpost_registry::serve_loopback_test(
        listener,
        registry,
        http_config,
        stopped,
    ));
    let http = reqwest::Client::new();
    let (github_handle, github_pubkey, github_signature) = signed_binding("/github/alice", 9);

    let github_response = http
        .post(format!("{base}/v1/identity/challenge"))
        .json(&serde_json::json!({
            "provider": "github",
            "handle": github_handle.as_path(),
            "pubkey": pigeonpost_registry::registry::hex(&github_pubkey),
            "signature": pigeonpost_registry::registry::hex(&github_signature),
            "pkce_challenge": pkce_s256(&"a".repeat(43)).unwrap()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(github_response.status(), 200);
    assert_eq!(github_response.headers()["cache-control"], "no-store");
    let github_body = github_response.text().await.unwrap();
    assert!(!github_body.contains("github-client-secret-must-not-leak"));
    let github: serde_json::Value = serde_json::from_str(&github_body).unwrap();
    assert_eq!(github["client_id"], "github-public-id");
    assert_eq!(
        github["authorization_endpoint"],
        GITHUB_AUTHORIZATION_ENDPOINT
    );
    assert_eq!(github["response_type"], "code");
    assert_eq!(github["response_mode"], "query");
    assert_eq!(github["challenge_parameter"], "state");
    assert_eq!(github["pkce_method"], "S256");
    assert_eq!(github["scopes"], serde_json::json!([]));

    let legacy_alias = http
        .post(format!("{base}/v1/identity/challenge"))
        .json(&serde_json::json!({
            "provider": "github",
            "handle": "/gh/alice",
            "pubkey": pigeonpost_registry::registry::hex(&github_pubkey),
            "signature": pigeonpost_registry::registry::hex(&github_signature),
            "pkce_challenge": pkce_s256(&"a".repeat(43)).unwrap()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        legacy_alias.status(),
        400,
        "legacy /gh must not start a new claim flow"
    );

    let (google_handle, google_pubkey, google_signature) =
        signed_binding("/google/104729183746501928374", 10);
    let google: serde_json::Value = http
        .post(format!("{base}/v1/identity/challenge"))
        .json(&serde_json::json!({
            "provider": "google",
            "handle": google_handle.as_path(),
            "pubkey": pigeonpost_registry::registry::hex(&google_pubkey),
            "signature": pigeonpost_registry::registry::hex(&google_signature),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        google["client_id"],
        "google-public-id.apps.googleusercontent.com"
    );
    assert_eq!(
        google["authorization_endpoint"],
        GOOGLE_AUTHORIZATION_ENDPOINT
    );
    assert_eq!(google["response_type"], "id_token");
    assert_eq!(google["response_mode"], "fragment");
    assert_eq!(google["challenge_parameter"], "nonce");
    assert!(google["pkce_method"].is_null());
    assert_eq!(google["scopes"], serde_json::json!(["openid", "profile"]));
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn code_is_exchanged_with_pkce_and_never_used_as_the_bearer_assertion() {
    let (base, stub) = oauth_stub().await;
    let registry = with_test_trace(
        Registry::in_memory(config())
            .unwrap()
            .with_provider(Box::new(
                GithubProvider::new("client-id", "client-secret")
                    .with_endpoints(format!("{base}/token"), format!("{base}/user")),
            )),
    );
    let verifier = "a".repeat(43);
    let (handle, pubkey, signature) = signed_binding("/github/alice", 1);
    let challenge = registry
        .issue_identity_challenge(
            IdentityChallengeProvider::Github,
            &handle,
            &pubkey,
            &signature,
            Some(&pkce_s256(&verifier).unwrap()),
        )
        .unwrap();
    let proof = ProofPayload::Github {
        code: "authorization-code".into(),
        code_verifier: verifier.clone(),
        state: challenge.value,
    };
    registry
        .register(&handle, &pubkey, &signature, &proof, source())
        .await
        .unwrap();
    assert_eq!(
        registry.entry(0).unwrap().handle_binding().unwrap().2,
        "github:12345678"
    );

    let bodies = stub.token_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("code=authorization-code"));
    assert!(bodies[0].contains(&format!("code_verifier={verifier}")));
    drop(bodies);
    let authorizations = stub.user_authorizations.lock().unwrap();
    assert_eq!(authorizations.as_slice(), ["Bearer provider-access-token"]);
    assert!(!authorizations[0].contains("authorization-code"));
}

#[tokio::test]
async fn state_is_single_use_and_wrong_pkce_is_rejected_before_provider_io() {
    let (base, stub) = oauth_stub().await;
    let registry = with_test_trace(
        Registry::in_memory(config())
            .unwrap()
            .with_provider(Box::new(
                GithubProvider::new("client-id", "client-secret")
                    .with_endpoints(format!("{base}/token"), format!("{base}/user")),
            )),
    );
    let correct = "a".repeat(43);
    let (handle, pubkey, signature) = signed_binding("/github/alice", 1);
    let challenge = registry
        .issue_identity_challenge(
            IdentityChallengeProvider::Github,
            &handle,
            &pubkey,
            &signature,
            Some(&pkce_s256(&correct).unwrap()),
        )
        .unwrap();

    let wrong = registry
        .register(
            &handle,
            &pubkey,
            &signature,
            &ProofPayload::Github {
                code: "authorization-code".into(),
                code_verifier: "b".repeat(43),
                state: challenge.value.clone(),
            },
            source(),
        )
        .await
        .unwrap_err();
    assert!(matches!(wrong, RegistryError::ProofRejected(_)));
    assert!(stub.token_bodies.lock().unwrap().is_empty());

    // A mismatched PKCE attempt does not consume the state; the exact verifier may still use it.
    let proof = ProofPayload::Github {
        code: "authorization-code".into(),
        code_verifier: correct,
        state: challenge.value,
    };
    let first = registry
        .register(&handle, &pubkey, &signature, &proof, source())
        .await
        .unwrap();
    // Challenge consumption and append commit together. Recover the exact committed result when
    // a caller retries after losing the first HTTP response, without contacting GitHub again.
    let replay = registry
        .register(&handle, &pubkey, &signature, &proof, source())
        .await
        .unwrap();
    assert!(!replay.appended);
    assert_eq!(replay.index, first.index);
    assert_eq!(registry.size().unwrap(), 1);
    assert_eq!(stub.token_bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn challenge_bound_to_one_key_cannot_be_won_by_a_second_key() {
    let (base, stub) = oauth_stub().await;
    let registry = with_test_trace(
        Registry::in_memory(config())
            .unwrap()
            .with_provider(Box::new(
                GithubProvider::new("client-id", "client-secret")
                    .with_endpoints(format!("{base}/token"), format!("{base}/user")),
            )),
    );
    let verifier = "a".repeat(43);
    let (handle, owner_pubkey, owner_signature) = signed_binding("/github/alice", 21);
    let (_, racing_pubkey, racing_signature) = signed_binding("/github/alice", 22);
    let challenge = registry
        .issue_identity_challenge(
            IdentityChallengeProvider::Github,
            &handle,
            &owner_pubkey,
            &owner_signature,
            Some(&pkce_s256(&verifier).unwrap()),
        )
        .unwrap();

    let raced = registry
        .register(
            &handle,
            &racing_pubkey,
            &racing_signature,
            &ProofPayload::Github {
                code: "authorization-code".into(),
                code_verifier: verifier.clone(),
                state: challenge.value.clone(),
            },
            source(),
        )
        .await
        .unwrap_err();
    assert!(matches!(raced, RegistryError::ProofRejected(_)));
    assert!(stub.token_bodies.lock().unwrap().is_empty());

    registry
        .register(
            &handle,
            &owner_pubkey,
            &owner_signature,
            &ProofPayload::Github {
                code: "authorization-code".into(),
                code_verifier: verifier,
                state: challenge.value,
            },
            source(),
        )
        .await
        .unwrap();
    assert_eq!(stub.token_bodies.lock().unwrap().len(), 1);
}
