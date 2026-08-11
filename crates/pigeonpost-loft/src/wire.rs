//! Request and response bodies for the loft HTTP API.
//!
//! JSON throughout: volume is small, and a format an operator can read with `curl` is worth more
//! than a few bytes on the wire at this scale (`docs/sds.md` §3).

use pigeonpost_core::{
    envelope::Wrap,
    record::{AgentRecord, RotationRecord},
    FetchAuth, RecipientPolicy,
};
use serde::{Deserialize, Serialize};

/// Protocol-wide ceiling for one serialized Pigeonpost event.
///
/// This lives in the feature-independent wire module so client-only dependency graphs validate
/// durable outbox payloads against the same bound as a server without enabling server storage or
/// compliance code.
pub const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

/// Largest retention period a conforming loft may advertise.
///
/// Clients persist this wire value when they add a loft and continue draining a removed route
/// through the exact advertised period. Keep the ceiling available without the `server` feature so
/// client-only dependency graphs can validate an untrusted `/v1/info` response without linking
/// storage or compliance code.
pub const MAX_RETENTION_DAYS: u64 = 3_650;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub wrap: Wrap,
    /// Loft-bound token presentation, hex. Required when the recipient's policy demands one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl core::fmt::Debug for PublishRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Both the optional presentation and the wrap are private request material. Keep this
        // formatter deliberately content-free so future fields are redacted by default too.
        formatter.write_str("PublishRequest { .. }")
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishResponse {
    /// Hex message id. Stable across lofts, so a client can dedupe on it.
    pub id: String,
    /// False when the loft already held this message — still a success.
    pub stored: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    pub auth: FetchAuth,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchResponse {
    pub events: Vec<Wrap>,
    /// Where to resume. Unchanged from the request when nothing was waiting.
    pub next_cursor: u64,
    /// True when more mail is waiting right now — drain again rather than sleeping.
    pub more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRequest {
    pub policy: RecipientPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRecordRequest {
    pub record: AgentRecord,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationRecordRequest {
    pub record: RotationRecord,
}

/// What the prober and the directory read. Everything here is public by design.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoResponse {
    pub software: String,
    pub version: String,
    pub protocol: String,
    /// The loft's own key. Senders bind token presentations to it; clients bind fetch proofs.
    pub pubkey: String,
    /// Exact canonical origin covered by fetch credentials.
    pub origin: String,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    /// `used / capacity`, the figure directory weighting divides on.
    pub utilization: f64,
    pub retention_days: u64,
    /// Directory-advertised default admission posture. v0.2 supports open admission only.
    pub open: bool,
    /// Directory-advertised service-wide work floor. Recipient-signed policy may require more.
    pub pow_floor: u32,
    pub max_event_bytes: usize,
    pub event_count: u64,
    /// Advertised, not measured — a budget we choose (`docs/capacity.md`).
    pub accepting: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_request_debug_is_content_free() {
        let request = PublishRequest {
            wrap: Wrap {
                version: 3,
                ephemeral_pubkey: [0xa1; 32],
                recipient: [0xb2; 32],
                nonce: [0xc3; 24],
                ciphertext: b"wrap-material-canary".to_vec(),
                created_at: 1,
                signature: [0xd4; 64],
                pow_nonce: 2,
                attribution: None,
            },
            token: Some("presentation-canary".into()),
        };

        let rendered = format!("{request:?}");
        assert_eq!(rendered, "PublishRequest { .. }");
        assert!(!rendered.contains("presentation-canary"));
        assert!(!rendered.contains("wrap-material-canary"));
    }
}
