use std::net::{IpAddr, Ipv4Addr};

use pigeonpost_compliance::{CompletionStatus, DisclosureCompletion, DisclosureIntent};
use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
use pigeonpost_compliance_seal::{
    IdentityProvider, IdentityTraceRecord, NetworkOperation, TraceIp, TraceRecord,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: u8,
    key_id: KeyIdVector,
    network_trace: NetworkTraceVector,
    identity_trace: IdentityTraceVector,
    disclosure_intent: DisclosureIntentVector,
    disclosure_completion: DisclosureCompletionVector,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyIdVector {
    purpose: String,
    jurisdiction: String,
    authority_hex: String,
    epoch_start_ms: u64,
    generation: u32,
    encoded_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkTraceVector {
    timestamp_ms: u64,
    node_id_hex: String,
    source_ip: String,
    source_port: u16,
    event_id_hex: String,
    recipient_hex: String,
    size_bytes: u32,
    encoded_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityTraceVector {
    timestamp_ms: u64,
    node_id_hex: String,
    correlation_id_hex: String,
    provider: String,
    provider_subject: String,
    encoded_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisclosureIntentVector {
    request_id_hex: String,
    timestamp_ms: u64,
    order_commitment_hex: String,
    requester_commitment_hex: String,
    selector_commitment_hex: String,
    approver_commitment_hexes: Vec<String>,
    encoded_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisclosureCompletionVector {
    request_id_hex: String,
    timestamp_ms: u64,
    status: String,
    record_count: u32,
    result_commitment_hex: String,
    encoded_hex: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!(
        "../../pigeonpost-compliance-format/fixtures/compliance-v1.json"
    ))
    .expect("the tracked compliance fixture is strict JSON")
}

fn hex<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2);
    let mut out = [0u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    out
}

fn encoded_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn key_id(vector: &KeyIdVector) -> ComplianceKeyId {
    let purpose = match vector.purpose.as_str() {
        "network_trace" => CompliancePurpose::NetworkTrace,
        value => panic!("unsupported fixture purpose {value}"),
    };
    let jurisdiction = match vector.jurisdiction.as_str() {
        "test" => Jurisdiction::Test,
        value => panic!("unsupported fixture jurisdiction {value}"),
    };
    ComplianceKeyId::new(
        purpose,
        jurisdiction,
        hex(&vector.authority_hex),
        vector.epoch_start_ms,
        vector.generation,
    )
}

#[test]
fn fixed_trace_key_and_disclosure_vectors_are_stable() {
    let fixture = fixture();
    assert_eq!(fixture.version, 1);
    let key_id = key_id(&fixture.key_id);
    let key_bytes = key_id.encode().unwrap();

    let source_ip: IpAddr = fixture.network_trace.source_ip.parse().unwrap();
    let IpAddr::V4(source_ip) = source_ip else {
        panic!("network fixture must use IPv4")
    };
    let network = TraceRecord {
        jurisdiction: key_id.jurisdiction,
        operation: NetworkOperation::Publish,
        timestamp_ms: fixture.network_trace.timestamp_ms,
        node_id: hex(&fixture.network_trace.node_id_hex),
        source_ip: TraceIp::V4(Ipv4Addr::from(source_ip.octets())),
        source_port: fixture.network_trace.source_port,
        event_id: Some(hex(&fixture.network_trace.event_id_hex)),
        recipient: Some(hex(&fixture.network_trace.recipient_hex)),
        owner: None,
        size_bytes: fixture.network_trace.size_bytes,
        correlation_id: None,
    };
    let network_bytes = network.encode().unwrap();

    let identity = IdentityTraceRecord {
        jurisdiction: key_id.jurisdiction,
        timestamp_ms: fixture.identity_trace.timestamp_ms,
        node_id: hex(&fixture.identity_trace.node_id_hex),
        correlation_id: hex(&fixture.identity_trace.correlation_id_hex),
        provider: match fixture.identity_trace.provider.as_str() {
            "oauth2" => IdentityProvider::Oauth2,
            value => panic!("unsupported fixture provider {value}"),
        },
        provider_subject: fixture.identity_trace.provider_subject.clone(),
    };
    let identity_bytes = identity.encode().unwrap();

    let intent = DisclosureIntent {
        request_id: hex(&fixture.disclosure_intent.request_id_hex),
        timestamp_ms: fixture.disclosure_intent.timestamp_ms,
        jurisdiction: key_id.jurisdiction,
        purpose: key_id.purpose,
        key_ids: vec![key_id],
        order_commitment: hex(&fixture.disclosure_intent.order_commitment_hex),
        requester_commitment: hex(&fixture.disclosure_intent.requester_commitment_hex),
        selector_commitment: hex(&fixture.disclosure_intent.selector_commitment_hex),
        approver_commitments: fixture
            .disclosure_intent
            .approver_commitment_hexes
            .iter()
            .map(|value| hex(value))
            .collect(),
    };
    let intent_bytes = intent.encode().unwrap();

    let completion = DisclosureCompletion {
        request_id: hex(&fixture.disclosure_completion.request_id_hex),
        timestamp_ms: fixture.disclosure_completion.timestamp_ms,
        status: match fixture.disclosure_completion.status.as_str() {
            "succeeded" => CompletionStatus::Succeeded,
            value => panic!("unsupported fixture completion status {value}"),
        },
        record_count: fixture.disclosure_completion.record_count,
        result_commitment: hex(&fixture.disclosure_completion.result_commitment_hex),
    };
    let completion_bytes = completion.encode().unwrap();

    let actual = [
        (
            "key_id",
            encoded_hex(&key_bytes),
            &fixture.key_id.encoded_hex,
        ),
        (
            "network_trace",
            encoded_hex(&network_bytes),
            &fixture.network_trace.encoded_hex,
        ),
        (
            "identity_trace",
            encoded_hex(&identity_bytes),
            &fixture.identity_trace.encoded_hex,
        ),
        (
            "disclosure_intent",
            encoded_hex(&intent_bytes),
            &fixture.disclosure_intent.encoded_hex,
        ),
        (
            "disclosure_completion",
            encoded_hex(&completion_bytes),
            &fixture.disclosure_completion.encoded_hex,
        ),
    ];
    for (name, encoded, expected) in actual {
        assert_eq!(encoded, *expected, "{name} canonical bytes changed");
    }

    assert_eq!(ComplianceKeyId::decode(&key_bytes).unwrap(), key_id);
    assert_eq!(TraceRecord::decode(&network_bytes).unwrap(), network);
    assert_eq!(
        IdentityTraceRecord::decode(&identity_bytes).unwrap(),
        identity
    );
    assert_eq!(DisclosureIntent::decode(&intent_bytes).unwrap(), intent);
    assert_eq!(
        DisclosureCompletion::decode(&completion_bytes).unwrap(),
        completion
    );
}
