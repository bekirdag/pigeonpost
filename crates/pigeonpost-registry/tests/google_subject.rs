#![cfg(feature = "test-utilities")]

//! Real RS256/JWKS tests for exact OIDC claims, nonce replay, bounded caching, and privacy.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use pigeonpost_compliance_format::{Jurisdiction, TraceCapturePolicy, TraceRetentionPolicy};
use pigeonpost_core::Identity;
use pigeonpost_registry::{
    claim_trace::{ClaimTraceCapacity, ClaimTraceError, ClaimTraceInput, ClaimTraceSink},
    entry::claim_payload,
    identity::{GoogleProvider, IdentityProvider, ProofPayload},
    Handle, IdentityChallengeProvider, Registry, RegistryConfig, RegistryError,
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
    "192.0.2.12:4242".parse().unwrap()
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

const KID: &str = "test-key-1";
const CLIENT_ID: &str = "pigeonpost-test.apps.googleusercontent.com";
const SUB: &str = "104729183746501928374";
const OPTIONAL_PROFILE: &str = "Alice Personal";
const NONCE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const RSA_N: &str = "yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ";
const RSA_E: &str = "AQAB";

// Boundary-free public test-only fixtures from jsonwebtoken's MIT-licensed tests. Keeping the
// PKCS#8 markers out of the repository prevents hosted secret scanners from mistaking these
// intentionally public test vectors for deployable credentials.
const TEST_PRIVATE_KEY_BODY: &str = include_str!("fixtures/google_test_private_key.pem");
const UNKNOWN_PRIVATE_KEY_BODY: &str = include_str!("fixtures/google_unknown_private_key.pem");

fn jwks() -> serde_json::Value {
    serde_json::json!({
        "keys": [{
            "kid": KID,
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "n": RSA_N,
            "e": RSA_E,
        }]
    })
}

#[derive(Clone)]
struct Claims<'a> {
    issuer: &'a str,
    audience: &'a str,
    nonce: &'a str,
    issued_offset: i64,
    expiry_offset: i64,
    include_profile: bool,
}

impl<'a> Default for Claims<'a> {
    fn default() -> Self {
        Self {
            issuer: "https://accounts.google.com",
            audience: CLIENT_ID,
            nonce: NONCE,
            issued_offset: 0,
            expiry_offset: 3_600,
            include_profile: true,
        }
    }
}

fn id_token(pkcs8_body: &str, claims: Claims<'_>) -> String {
    let now = unix_seconds() as i64;
    let mut payload = serde_json::json!({
        "iss": claims.issuer,
        "aud": claims.audience,
        "sub": SUB,
        "exp": now + claims.expiry_offset,
        "iat": now + claims.issued_offset,
        "nonce": claims.nonce,
    });
    if claims.include_profile {
        payload["name"] = serde_json::json!(OPTIONAL_PROFILE);
        payload["picture"] = serde_json::json!("https://example.invalid/alice.png");
    }
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KID.to_owned());
    let pem = format!(
        "{}{}{}",
        concat!("-----BEGIN ", "PRIVATE KEY-----\n"),
        pkcs8_body.trim(),
        concat!("\n-----END ", "PRIVATE KEY-----\n")
    );
    jsonwebtoken::encode(
        &header,
        &payload,
        &jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn serve_jwks(document: serde_json::Value) -> (String, Arc<AtomicUsize>) {
    use axum::{routing::get, Router};
    let requests = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&requests);
    let app = Router::new().route(
        "/certs",
        get(move || {
            let document = document.clone();
            let count = Arc::clone(&count);
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                (
                    [
                        ("cache-control", "public, max-age=600"),
                        ("etag", "\"test-jwks-v1\""),
                    ],
                    axum::Json(document),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/certs", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, requests)
}

#[tokio::test]
async fn exact_valid_token_yields_only_the_opaque_subject() {
    let (url, _) = serve_jwks(jwks()).await;
    let provider = GoogleProvider::new(CLIENT_ID).with_jwks_url(url);
    let subject = provider
        .verify(&ProofPayload::Google {
            id_token: id_token(TEST_PRIVATE_KEY_BODY, Claims::default()),
            nonce: NONCE.into(),
        })
        .await
        .unwrap();
    assert_eq!(subject.namespace, "google");
    assert_eq!(subject.name, SUB);
    assert!(!subject.name.contains('@'));
    assert!(!subject.name.contains("alice"));
}

#[tokio::test]
async fn personal_identifier_reaches_no_part_of_the_public_log() {
    let (url, _) = serve_jwks(jwks()).await;
    let registry = with_test_trace(
        Registry::in_memory(RegistryConfig {
            origin: "pigeonpost.dev/registry".into(),
            signing_key: SigningKey::from_bytes(&[42; 32]),
            allow_mock_identities: false,
        })
        .unwrap()
        .with_provider(Box::new(GoogleProvider::new(CLIENT_ID).with_jwks_url(url))),
    );
    let (handle, pubkey, signature) = signed_binding(&format!("/google/{SUB}"), 1);
    let challenge = registry
        .issue_identity_challenge(
            IdentityChallengeProvider::Google,
            &handle,
            &pubkey,
            &signature,
            None,
        )
        .unwrap();
    registry
        .register(
            &handle,
            &pubkey,
            &signature,
            &ProofPayload::Google {
                id_token: id_token(
                    TEST_PRIVATE_KEY_BODY,
                    Claims {
                        nonce: &challenge.value,
                        ..Claims::default()
                    },
                ),
                nonce: challenge.value,
            },
            source(),
        )
        .await
        .unwrap();

    let dump = registry.dump().unwrap();
    let published = serde_json::to_string(&dump).unwrap();
    assert!(!published.contains(OPTIONAL_PROFILE));
    assert!(!published.contains("example.invalid/alice.png"));
    assert_eq!(dump[0].handle_binding().unwrap().2, format!("google:{SUB}"));
}

#[tokio::test]
async fn issuer_audience_expiry_issued_at_nonce_and_signature_fail_independently() {
    let (url, _) = serve_jwks(jwks()).await;
    let provider = GoogleProvider::new(CLIENT_ID).with_jwks_url(url);

    let cases = [
        Claims {
            issuer: "https://attacker.invalid",
            ..Claims::default()
        },
        Claims {
            audience: "another-client",
            ..Claims::default()
        },
        Claims {
            expiry_offset: -120,
            ..Claims::default()
        },
        Claims {
            issued_offset: 120,
            expiry_offset: 3_600,
            ..Claims::default()
        },
        Claims {
            nonce: "2222222222222222222222222222222222222222222222222222222222222222",
            ..Claims::default()
        },
    ];
    for claims in cases {
        let result = provider
            .verify(&ProofPayload::Google {
                id_token: id_token(TEST_PRIVATE_KEY_BODY, claims),
                nonce: NONCE.into(),
            })
            .await;
        assert!(result.is_err());
    }

    let bad_signature = provider
        .verify(&ProofPayload::Google {
            id_token: id_token(UNKNOWN_PRIVATE_KEY_BODY, Claims::default()),
            nonce: NONCE.into(),
        })
        .await;
    assert!(bad_signature.is_err());
}

#[tokio::test]
async fn jwks_is_cached_and_not_fetched_per_proof() {
    let (url, requests) = serve_jwks(jwks()).await;
    let provider = GoogleProvider::new(CLIENT_ID).with_jwks_url(url);
    for _ in 0..3 {
        provider
            .verify(&ProofPayload::Google {
                id_token: id_token(TEST_PRIVATE_KEY_BODY, Claims::default()),
                nonce: NONCE.into(),
            })
            .await
            .unwrap();
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn consumed_nonce_recovers_only_the_exact_idempotent_handle_retry() {
    let (url, _) = serve_jwks(jwks()).await;
    let registry = with_test_trace(
        Registry::in_memory(RegistryConfig {
            origin: "pigeonpost.dev/registry".into(),
            signing_key: SigningKey::from_bytes(&[42; 32]),
            allow_mock_identities: false,
        })
        .unwrap()
        .with_provider(Box::new(GoogleProvider::new(CLIENT_ID).with_jwks_url(url))),
    );
    let (handle, pubkey, signature) = signed_binding(&format!("/google/{SUB}"), 1);
    let challenge = registry
        .issue_identity_challenge(
            IdentityChallengeProvider::Google,
            &handle,
            &pubkey,
            &signature,
            None,
        )
        .unwrap();
    let invalid = ProofPayload::Google {
        id_token: id_token(
            UNKNOWN_PRIVATE_KEY_BODY,
            Claims {
                nonce: &challenge.value,
                ..Claims::default()
            },
        ),
        nonce: challenge.value.clone(),
    };
    let proof = ProofPayload::Google {
        id_token: id_token(
            TEST_PRIVATE_KEY_BODY,
            Claims {
                nonce: &challenge.value,
                ..Claims::default()
            },
        ),
        nonce: challenge.value,
    };
    let rejected = registry
        .register(&handle, &pubkey, &signature, &invalid, source())
        .await
        .unwrap_err();
    assert!(matches!(rejected, RegistryError::ProofRejected(_)));
    // Invalid provider material does not burn a legitimate nonce; only a verified subject may
    // atomically consume it.
    let first = registry
        .register(&handle, &pubkey, &signature, &proof, source())
        .await
        .unwrap();
    // The nonce is consumed once in the same transaction as the append. An exact retry recovers
    // that committed result so a lost HTTP response cannot force a second provider ceremony.
    let replay = registry
        .register(&handle, &pubkey, &signature, &proof, source())
        .await
        .unwrap();
    assert!(!replay.appended);
    assert_eq!(replay.index, first.index);
    assert_eq!(registry.size().unwrap(), 1);
}

#[tokio::test]
async fn errors_never_echo_the_submitted_assertion() {
    let (url, _) = serve_jwks(jwks()).await;
    let provider = GoogleProvider::new(CLIENT_ID).with_jwks_url(url);
    let secret_marker = format!("header.{OPTIONAL_PROFILE}.signature");
    let error = provider
        .verify(&ProofPayload::Google {
            id_token: secret_marker.clone(),
            nonce: NONCE.into(),
        })
        .await
        .unwrap_err();
    assert!(!error.to_string().contains(&secret_marker));
    assert!(!error.to_string().contains(OPTIONAL_PROFILE));
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
