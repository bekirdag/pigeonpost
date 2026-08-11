//! Conformance vectors.
//!
//! `docs/infrastructure.md` day-one commitment #5 promises a wire format documented well enough
//! for a clean-room implementation, and `docs/sds.md` §8 makes these vectors the contract. A
//! second implementation that reproduces every value below is compatible; one that does not, is
//! not.
//!
//! Deterministic values only — anything involving fresh randomness (envelope ciphertexts,
//! ephemeral keys) cannot be a fixed vector, so those are covered by round-trip properties in the
//! unit tests instead.
//!
//! **Changing a value in this file is a protocol break.** If a change here is not accompanied by
//! a version bump, the change is wrong.

use pigeonpost_compliance_format::{
    attribution_aad, attribution_signing_preimage, ComplianceKeyId, CompliancePurpose,
    Jurisdiction, ATTRIBUTION_BLOCK_VERSION,
};
use pigeonpost_core::{
    b32,
    envelope::{open, Wrap, ENVELOPE_VERSION, LEGACY_ENVELOPE_VERSION},
    keys::SuccessorCommitment,
    pow,
    token::Token,
    Address, AgentRecord, Identity, RotationRecord,
};
use serde::Deserialize;

const MAX_FIXTURE_BYTES: usize = 64 * 1024;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Seeded identities, so every vector below is reproducible from a known 32-byte seed.
fn identity(byte: u8) -> Identity {
    Identity::from_seed([byte; 32])
}

fn decode_hex<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2, "fixed vector has the wrong hex length");
    let mut bytes = [0u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).unwrap();
        bytes[index] = u8::from_str_radix(pair, 16).expect("fixed vector contains invalid hex");
    }
    bytes
}

fn fixture<T: serde::de::DeserializeOwned>(bytes: &'static str) -> T {
    assert!(
        bytes.len() <= MAX_FIXTURE_BYTES,
        "a conformance fixture must remain bounded"
    );
    serde_json::from_str(bytes).expect("published conformance fixture must decode")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressV1Fixture {
    vectors: Vec<AddressV1Vector>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressV1Vector {
    seed_hex: String,
    public_key_hex: String,
    address: String,
}

#[test]
fn vector_address_v1_derivation_and_reader_reject_drift() {
    let fixture: AddressV1Fixture = fixture(include_str!("fixtures/address-v1.json"));
    assert_eq!(
        fixture.vectors.len(),
        2,
        "the v1 address vector set is fixed"
    );
    for vector in fixture.vectors {
        let identity = Identity::from_seed(decode_hex(&vector.seed_hex));
        assert_eq!(
            identity.verifying_key().to_bytes(),
            decode_hex(&vector.public_key_hex)
        );
        assert_eq!(identity.address().as_str(), vector.address);

        let parsed = Address::parse(&vector.address).unwrap();
        assert!(parsed.matches(&identity.verifying_key()));

        let mut mutated = vector.address.into_bytes();
        let last = mutated.last_mut().unwrap();
        *last = if *last == b'0' { b'1' } else { b'0' };
        let mutated = String::from_utf8(mutated).unwrap();
        assert!(
            Address::parse(&mutated)
                .map(|address| !address.matches(&identity.verifying_key()))
                .unwrap_or(true),
            "a drifted address must not identify the fixture key"
        );
    }
}

#[test]
fn vector_base32_alphabet() {
    // Crockford, lowercase, no padding.
    assert_eq!(b32::encode(&[0x00]), "00");
    assert_eq!(b32::encode(&[0xff]), "zw");
    assert_eq!(b32::encode(&[0x00, 0x44, 0x32]), "01234");
    assert_eq!(
        b32::encode(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
        "04hmasw9nf6yy"
    );
}

#[test]
fn vector_successor_commitment() {
    let commitment = SuccessorCommitment::for_key(&identity(2).verifying_key());
    assert_eq!(
        hex(commitment.as_bytes()),
        "6a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f03827"
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrimitivesV1Fixture {
    pow: PowVector,
    /// Retained so the published v1 fixture remains parseable; v2 origin-bound token vectors live
    /// in their own fixture and are the current compatibility contract.
    #[serde(rename = "token")]
    _legacy_token: serde_json::Value,
    agent_record: AgentRecordVector,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PowVector {
    id_hex: String,
    nonce_zero: u64,
    nonce_zero_work: u32,
    difficulty: u32,
    first_valid_nonce: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenV2Vector {
    secret_hex: String,
    label: String,
    token_hex: String,
    loft_a_hex: String,
    loft_a_origin: String,
    presentation_a_hex: String,
    loft_b_hex: String,
    loft_b_origin: String,
    presentation_b_hex: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRecordVector {
    identity_seed_hex: String,
    successor_seed_hex: String,
    record: AgentRecord,
}

fn primitive_fixture() -> PrimitivesV1Fixture {
    fixture(include_str!("fixtures/primitives-v1.json"))
}

fn token_v2_fixture() -> TokenV2Vector {
    fixture(include_str!("fixtures/token-origin-v2.json"))
}

#[test]
fn vector_proof_of_work() {
    let vector = primitive_fixture().pow;
    let id = decode_hex(&vector.id_hex);
    assert_eq!(vector.nonce_zero, 0);
    assert_eq!(pow::work(&id, vector.nonce_zero), vector.nonce_zero_work);
    assert_eq!(
        pow::mine(&id, vector.difficulty, vector.first_valid_nonce + 1),
        Some(vector.first_valid_nonce)
    );
    assert!(pow::verify(&id, vector.first_valid_nonce, vector.difficulty).is_ok());

    let mut changed_id = id;
    changed_id[0] ^= 1;
    assert!(pow::verify(&changed_id, vector.first_valid_nonce, vector.difficulty).is_err());
    assert!(pow::verify(&id, vector.first_valid_nonce + 1, vector.difficulty).is_err());
}

#[test]
fn vector_capability_token() {
    let vector = token_v2_fixture();
    let secret = decode_hex(&vector.secret_hex);
    let loft_a = decode_hex(&vector.loft_a_hex);
    let loft_b = decode_hex(&vector.loft_b_hex);
    let token = Token::mint(&secret, &vector.label);
    assert_eq!(token.to_hex(), vector.token_hex);
    assert_eq!(
        hex(token
            .presentation(&loft_a, &vector.loft_a_origin)
            .unwrap()
            .as_bytes()),
        vector.presentation_a_hex
    );
    assert_eq!(
        hex(token
            .presentation(&loft_b, &vector.loft_b_origin)
            .unwrap()
            .as_bytes()),
        vector.presentation_b_hex
    );
    assert_ne!(
        Token::mint(&secret, "readme-mutated").to_hex(),
        vector.token_hex
    );
    assert_ne!(
        hex(token
            .presentation(&loft_a, &vector.loft_a_origin)
            .unwrap()
            .as_bytes()),
        vector.presentation_b_hex
    );
}

#[test]
fn vector_agent_record_signing_and_reader_reject_drift() {
    let vector = primitive_fixture().agent_record;
    let operating = Identity::from_seed(decode_hex(&vector.identity_seed_hex));
    let successor = Identity::from_seed(decode_hex(&vector.successor_seed_hex));
    let commitment = SuccessorCommitment::for_key(&successor.verifying_key());
    // The original fixture remains the deployed v1 read contract.
    assert_eq!(vector.record.version, 1);
    assert!(vector.record.verify(&operating.address()).is_ok());

    // Writers moved to v2 when recipient-selected attribution scope became signed discovery data.
    let generated = AgentRecord::with_pow(
        &operating,
        &commitment,
        vector.record.seq,
        vector.record.lofts.clone(),
        vector.record.pow_min,
    );
    assert_eq!(generated.version, 2);
    assert_eq!(generated.pubkey, vector.record.pubkey);
    assert_eq!(generated.successor_hash, vector.record.successor_hash);
    assert_eq!(generated.seq, vector.record.seq);
    assert_eq!(generated.lofts, vector.record.lofts);
    assert_eq!(generated.pow_min, vector.record.pow_min);
    assert_eq!(generated.attribution_requirement, None);
    assert_eq!(
        generated.signature,
        [
            132, 56, 200, 24, 8, 139, 93, 236, 202, 26, 82, 235, 147, 112, 111, 41, 184, 104, 71,
            239, 86, 231, 23, 75, 214, 134, 248, 168, 255, 248, 48, 160, 78, 101, 74, 85, 34, 89,
            93, 168, 123, 67, 211, 128, 255, 119, 197, 103, 53, 162, 132, 121, 181, 170, 203, 168,
            231, 161, 60, 149, 108, 216, 41, 13,
        ]
    );
    assert!(generated.verify(&operating.address()).is_ok());

    let mut changed = generated;
    changed.seq += 1;
    assert!(changed.verify(&operating.address()).is_err());
    let mut changed = vector.record;
    changed.signature[0] ^= 1;
    assert!(changed.verify(&operating.address()).is_err());
}

#[test]
fn vector_rotation_record_signing_is_stable() {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RotationV2Fixture {
        activated_at: u64,
        grace_until: u64,
        outgoing_signature_hex: String,
        incoming_signature_hex: String,
    }

    let fixture: RotationV2Fixture = fixture(include_str!("fixtures/rotation-v2.json"));
    let outgoing = identity(1);
    let incoming = identity(2);
    let next = SuccessorCommitment::for_key(&identity(3).verifying_key());

    let a = RotationRecord::new(&outgoing, &incoming, &next, 1, fixture.activated_at).unwrap();
    let b = RotationRecord::new(&outgoing, &incoming, &next, 1, fixture.activated_at).unwrap();
    assert_eq!(a, b, "Ed25519 rotation signing is deterministic");
    assert_eq!(a.grace_until, fixture.grace_until);
    assert_eq!(hex(&a.outgoing_signature), fixture.outgoing_signature_hex);
    assert_eq!(hex(&a.incoming_signature), fixture.incoming_signature_hex);

    let pinned = SuccessorCommitment::for_key(&incoming.verifying_key());
    assert_eq!(
        a.verify(&pinned, 0, fixture.activated_at).unwrap().incoming,
        incoming.verifying_key()
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeV2ReadFixture {
    sender_seed_hex: String,
    recipient_seed_hex: String,
    expected_sender_pubkey_hex: String,
    expected_body: String,
    expected_id_hex: String,
    wrap: Wrap,
}

#[test]
fn vector_envelope_v2_fixed_wire_reader_and_mutation_failures() {
    let raw = include_str!("fixtures/envelope-v2-read.json");
    let fixture: EnvelopeV2ReadFixture = fixture(raw);
    assert_eq!(fixture.wrap.version, LEGACY_ENVELOPE_VERSION);

    let sender = Identity::from_seed(decode_hex(&fixture.sender_seed_hex));
    let recipient = Identity::from_seed(decode_hex(&fixture.recipient_seed_hex));
    assert_eq!(
        sender.verifying_key().to_bytes(),
        decode_hex(&fixture.expected_sender_pubkey_hex)
    );
    assert_eq!(fixture.wrap.recipient, recipient.verifying_key().to_bytes());
    assert_eq!(hex(&fixture.wrap.id()), fixture.expected_id_hex);

    let (opened_sender, body) = open(&recipient, &fixture.wrap).unwrap();
    assert_eq!(opened_sender, sender.verifying_key());
    assert_eq!(body.as_str(), fixture.expected_body);

    let mut changed = fixture.wrap.clone();
    changed.created_at ^= 1;
    assert!(open(&recipient, &changed).is_err());
    let mut changed = fixture.wrap;
    changed.ciphertext[0] ^= 1;
    assert!(open(&recipient, &changed).is_err());

    let mut malformed: serde_json::Value = serde_json::from_str(raw).unwrap();
    malformed["wrap"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<EnvelopeV2ReadFixture>(malformed).is_err());
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeV3Fixture {
    compliance_key_id_hex: String,
    attribution_aad_hex: String,
    attribution_signing_preimage_hex: String,
    event_id_hex: String,
}

#[test]
fn vector_envelope_v3_public_context_and_event_id() {
    let fixture: EnvelopeV3Fixture = fixture(include_str!("fixtures/envelope-v3.json"));
    let key_id = ComplianceKeyId::new(
        CompliancePurpose::Attribution,
        Jurisdiction::Test,
        [0xA5; 32],
        0x0102_0304_0506_0708,
        0x090A_0B0C,
    );
    assert_eq!(
        hex(&key_id.encode().unwrap()),
        fixture.compliance_key_id_hex
    );
    assert_eq!(
        hex(&attribution_aad(
            ATTRIBUTION_BLOCK_VERSION,
            &key_id,
            &[1; 32],
            &[2; 32],
            &[3; 32],
            &[4; 32],
        )
        .unwrap()),
        fixture.attribution_aad_hex
    );
    assert_eq!(
        hex(&attribution_signing_preimage(
            ATTRIBUTION_BLOCK_VERSION,
            &key_id,
            &[1; 32],
            &[2; 32],
            &[3; 32],
            &[4; 32],
            5,
        )
        .unwrap()),
        fixture.attribution_signing_preimage_hex
    );

    let wrap = Wrap {
        version: ENVELOPE_VERSION,
        ephemeral_pubkey: [0x10; 32],
        recipient: [0x20; 32],
        nonce: [0x30; 24],
        ciphertext: vec![0x40, 0x41, 0x42],
        created_at: 0x0102_0304_0506_0708,
        signature: [0; 64],
        pow_nonce: 0,
        attribution: None,
    };
    assert_eq!(hex(&wrap.id()), fixture.event_id_hex);
}
