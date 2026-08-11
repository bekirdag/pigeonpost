#![cfg(not(feature = "test-utilities"))]

use pigeonpost_registry::identity::ProofPayload;

#[test]
fn production_wire_surface_rejects_mock_identity_proofs() {
    let encoded = br#"{"provider":"mock","name":"alice"}"#;
    assert!(serde_json::from_slice::<ProofPayload>(encoded).is_err());
}
