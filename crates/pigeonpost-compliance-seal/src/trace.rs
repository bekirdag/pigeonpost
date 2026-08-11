//! Canonical plaintext records accepted by the online trace sealer.
//!
//! These records are intentionally fixed-width. A decoder accepts one exact version and one
//! exact length, so concatenated records, extension bytes, and ambiguous optional fields cannot
//! be smuggled into a sealed segment. Source addresses are never exposed by `Debug`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use pigeonpost_compliance_format::Jurisdiction;
use zeroize::Zeroize;

use crate::error::{Result, SealError};

const TRACE_RECORD_VERSION: u8 = 1;
const IDENTITY_TRACE_RECORD_VERSION: u8 = 1;
const FLAG_EVENT_ID: u8 = 1 << 0;
const FLAG_RECIPIENT: u8 = 1 << 1;
const FLAG_OWNER: u8 = 1 << 2;
const FLAG_CORRELATION: u8 = 1 << 3;
const KNOWN_FLAGS: u8 = FLAG_EVENT_ID | FLAG_RECIPIENT | FLAG_OWNER | FLAG_CORRELATION;

/// Exact encoded length of a network trace record.
pub const TRACE_RECORD_LEN: usize = 195;
/// Maximum UTF-8 byte length of an identity-provider subject.
pub const IDENTITY_SUBJECT_MAX_LEN: usize = 128;
/// Exact encoded length of an identity trace record.
pub const IDENTITY_TRACE_RECORD_LEN: usize = 205;

/// A source address whose formatting deliberately never reveals the address.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraceIp {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

impl TraceIp {
    fn encode(self) -> (u8, [u8; 16]) {
        match self {
            Self::V4(address) => {
                let mut bytes = [0u8; 16];
                bytes[12..].copy_from_slice(&address.octets());
                (4, bytes)
            }
            Self::V6(address) => (6, address.octets()),
        }
    }

    fn decode(family: u8, bytes: [u8; 16]) -> Result<Self> {
        match family {
            4 if bytes[..12] == [0u8; 12] => Ok(Self::V4(Ipv4Addr::new(
                bytes[12], bytes[13], bytes[14], bytes[15],
            ))),
            4 => Err(SealError::InvalidRecord),
            6 => Ok(Self::V6(Ipv6Addr::from(bytes))),
            _ => Err(SealError::InvalidRecord),
        }
    }
}

impl From<IpAddr> for TraceIp {
    fn from(value: IpAddr) -> Self {
        match value {
            IpAddr::V4(address) => Self::V4(address),
            IpAddr::V6(address) => Self::V6(address),
        }
    }
}

impl core::fmt::Debug for TraceIp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("TraceIp(<withheld>)")
    }
}

/// Operation whose minimum network metadata is captured under `docs/law.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NetworkOperation {
    Publish = 1,
    Fetch = 2,
    PutAgent = 3,
    Claim = 4,
}

impl TryFrom<u8> for NetworkOperation {
    type Error = SealError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Publish),
            2 => Ok(Self::Fetch),
            3 => Ok(Self::PutAgent),
            4 => Ok(Self::Claim),
            _ => Err(SealError::InvalidRecord),
        }
    }
}

/// Network trace plaintext. Its `Debug` implementation withholds the source address.
#[derive(Clone, PartialEq, Eq)]
pub struct TraceRecord {
    pub jurisdiction: Jurisdiction,
    pub operation: NetworkOperation,
    pub timestamp_ms: u64,
    pub node_id: [u8; 32],
    pub source_ip: TraceIp,
    pub source_port: u16,
    pub event_id: Option<[u8; 32]>,
    pub recipient: Option<[u8; 32]>,
    pub owner: Option<[u8; 32]>,
    pub size_bytes: u32,
    pub correlation_id: Option<[u8; 32]>,
}

impl TraceRecord {
    /// Encode the record after enforcing the operation-specific field schema.
    pub fn encode(&self) -> Result<[u8; TRACE_RECORD_LEN]> {
        self.validate()?;
        let mut out = [0u8; TRACE_RECORD_LEN];
        let (family, address) = self.source_ip.encode();
        out[0] = TRACE_RECORD_VERSION;
        out[1] = self.jurisdiction.into();
        out[2] = self.operation as u8;
        out[3] = family;
        out[4..12].copy_from_slice(&self.timestamp_ms.to_be_bytes());
        out[12..44].copy_from_slice(&self.node_id);
        out[44..60].copy_from_slice(&address);
        out[60..62].copy_from_slice(&self.source_port.to_be_bytes());

        let mut flags = 0u8;
        if let Some(value) = self.event_id {
            flags |= FLAG_EVENT_ID;
            out[63..95].copy_from_slice(&value);
        }
        if let Some(value) = self.recipient {
            flags |= FLAG_RECIPIENT;
            out[95..127].copy_from_slice(&value);
        }
        if let Some(value) = self.owner {
            flags |= FLAG_OWNER;
            out[127..159].copy_from_slice(&value);
        }
        out[159..163].copy_from_slice(&self.size_bytes.to_be_bytes());
        if let Some(value) = self.correlation_id {
            flags |= FLAG_CORRELATION;
            out[163..195].copy_from_slice(&value);
        }
        out[62] = flags;
        Ok(out)
    }

    /// Decode exactly one canonical record. Trailing bytes and non-zero absent fields fail.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != TRACE_RECORD_LEN || bytes[0] != TRACE_RECORD_VERSION {
            return Err(SealError::InvalidRecord);
        }
        let flags = bytes[62];
        if flags & !KNOWN_FLAGS != 0 {
            return Err(SealError::InvalidRecord);
        }
        let read_optional =
            |flag: u8, range: core::ops::Range<usize>| -> Result<Option<[u8; 32]>> {
                let mut value = [0u8; 32];
                value.copy_from_slice(&bytes[range]);
                if flags & flag != 0 {
                    Ok(Some(value))
                } else if value == [0u8; 32] {
                    Ok(None)
                } else {
                    Err(SealError::InvalidRecord)
                }
            };
        let mut node_id = [0u8; 32];
        node_id.copy_from_slice(&bytes[12..44]);
        let mut address = [0u8; 16];
        address.copy_from_slice(&bytes[44..60]);
        let record = Self {
            jurisdiction: Jurisdiction::try_from(bytes[1]).map_err(|_| SealError::InvalidRecord)?,
            operation: NetworkOperation::try_from(bytes[2])?,
            timestamp_ms: u64::from_be_bytes(bytes[4..12].try_into().expect("fixed slice")),
            node_id,
            source_ip: TraceIp::decode(bytes[3], address)?,
            source_port: u16::from_be_bytes(bytes[60..62].try_into().expect("fixed slice")),
            event_id: read_optional(FLAG_EVENT_ID, 63..95)?,
            recipient: read_optional(FLAG_RECIPIENT, 95..127)?,
            owner: read_optional(FLAG_OWNER, 127..159)?,
            size_bytes: u32::from_be_bytes(bytes[159..163].try_into().expect("fixed slice")),
            correlation_id: read_optional(FLAG_CORRELATION, 163..195)?,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        if self.timestamp_ms == 0 || self.node_id == [0u8; 32] || self.source_port == 0 {
            return Err(SealError::InvalidRecord);
        }
        let shape_ok = match self.operation {
            NetworkOperation::Publish => {
                self.event_id.is_some()
                    && self.recipient.is_some()
                    && self.owner.is_none()
                    && self.correlation_id.is_none()
                    && self.size_bytes > 0
            }
            NetworkOperation::Fetch => {
                self.event_id.is_none()
                    && self.recipient.is_none()
                    && self.owner.is_some()
                    && self.correlation_id.is_none()
                    && self.size_bytes == 0
            }
            NetworkOperation::PutAgent => {
                self.event_id.is_none()
                    && self.recipient.is_none()
                    && self.owner.is_none()
                    && self.correlation_id.is_none()
                    && self.size_bytes == 0
            }
            NetworkOperation::Claim => {
                self.event_id.is_none()
                    && self.recipient.is_none()
                    && self.owner.is_none()
                    && self.correlation_id.is_some()
                    && self.size_bytes == 0
            }
        };
        if !shape_ok {
            return Err(SealError::InvalidRecord);
        }
        Ok(())
    }
}

impl core::fmt::Debug for TraceRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TraceRecord")
            .field("jurisdiction", &self.jurisdiction)
            .field("operation", &self.operation)
            .field("timestamp_ms", &self.timestamp_ms)
            .field("node_id", &"<withheld>")
            .field("source_ip", &"<withheld>")
            .field("source_port", &"<withheld>")
            .field("event_id", &"<withheld>")
            .field("recipient", &"<withheld>")
            .field("owner", &"<withheld>")
            .field("size_bytes", &self.size_bytes)
            .field("correlation_id", &"<withheld>")
            .finish()
    }
}

impl Drop for TraceRecord {
    fn drop(&mut self) {
        self.timestamp_ms = 0;
        self.node_id.zeroize();
        self.source_ip = TraceIp::V4(Ipv4Addr::UNSPECIFIED);
        self.source_port = 0;
        self.event_id.zeroize();
        self.recipient.zeroize();
        self.owner.zeroize();
        self.size_bytes = 0;
        self.correlation_id.zeroize();
    }
}

/// Identity-provider family. Provider-specific subject identifiers remain encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IdentityProvider {
    Oidc = 1,
    Saml = 2,
    LocalDirectory = 3,
    /// OAuth 2.0 authorization-code identity lookup, such as GitHub user identity.
    Oauth2 = 4,
}

impl TryFrom<u8> for IdentityProvider {
    type Error = SealError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Oidc),
            2 => Ok(Self::Saml),
            3 => Ok(Self::LocalDirectory),
            4 => Ok(Self::Oauth2),
            _ => Err(SealError::InvalidRecord),
        }
    }
}

/// Identity trace plaintext, deliberately separate from network records and source addresses.
#[derive(Clone, PartialEq, Eq)]
pub struct IdentityTraceRecord {
    pub jurisdiction: Jurisdiction,
    pub timestamp_ms: u64,
    pub node_id: [u8; 32],
    pub correlation_id: [u8; 32],
    pub provider: IdentityProvider,
    pub provider_subject: String,
}

impl IdentityTraceRecord {
    pub fn encode(&self) -> Result<[u8; IDENTITY_TRACE_RECORD_LEN]> {
        self.validate()?;
        let subject = self.provider_subject.as_bytes();
        let mut out = [0u8; IDENTITY_TRACE_RECORD_LEN];
        out[0] = IDENTITY_TRACE_RECORD_VERSION;
        out[1] = self.jurisdiction.into();
        out[2..10].copy_from_slice(&self.timestamp_ms.to_be_bytes());
        out[10..42].copy_from_slice(&self.node_id);
        out[42..74].copy_from_slice(&self.correlation_id);
        out[74] = self.provider as u8;
        out[75..77].copy_from_slice(&(subject.len() as u16).to_be_bytes());
        out[77..77 + subject.len()].copy_from_slice(subject);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != IDENTITY_TRACE_RECORD_LEN || bytes[0] != IDENTITY_TRACE_RECORD_VERSION {
            return Err(SealError::InvalidRecord);
        }
        let subject_len =
            u16::from_be_bytes(bytes[75..77].try_into().expect("fixed slice")) as usize;
        if subject_len == 0
            || subject_len > IDENTITY_SUBJECT_MAX_LEN
            || bytes[77 + subject_len..].iter().any(|byte| *byte != 0)
        {
            return Err(SealError::InvalidRecord);
        }
        let mut node_id = [0u8; 32];
        node_id.copy_from_slice(&bytes[10..42]);
        let mut correlation_id = [0u8; 32];
        correlation_id.copy_from_slice(&bytes[42..74]);
        let record = Self {
            jurisdiction: Jurisdiction::try_from(bytes[1]).map_err(|_| SealError::InvalidRecord)?,
            timestamp_ms: u64::from_be_bytes(bytes[2..10].try_into().expect("fixed slice")),
            node_id,
            correlation_id,
            provider: IdentityProvider::try_from(bytes[74])?,
            provider_subject: core::str::from_utf8(&bytes[77..77 + subject_len])
                .map_err(|_| SealError::InvalidRecord)?
                .to_owned(),
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        let subject_len = self.provider_subject.len();
        if self.timestamp_ms == 0
            || self.node_id == [0u8; 32]
            || self.correlation_id == [0u8; 32]
            || subject_len == 0
            || subject_len > IDENTITY_SUBJECT_MAX_LEN
            || self.provider_subject.chars().any(char::is_control)
        {
            return Err(SealError::InvalidRecord);
        }
        Ok(())
    }
}

impl core::fmt::Debug for IdentityTraceRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IdentityTraceRecord")
            .field("jurisdiction", &self.jurisdiction)
            .field("timestamp_ms", &self.timestamp_ms)
            .field("node_id", &"<withheld>")
            .field("correlation_id", &"<withheld>")
            .field("provider", &self.provider)
            .field("provider_subject", &"<withheld>")
            .finish()
    }
}

impl Drop for IdentityTraceRecord {
    fn drop(&mut self) {
        self.timestamp_ms = 0;
        self.node_id.zeroize();
        self.correlation_id.zeroize();
        self.provider_subject.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish() -> TraceRecord {
        TraceRecord {
            jurisdiction: Jurisdiction::Test,
            operation: NetworkOperation::Publish,
            timestamp_ms: 1,
            node_id: [1; 32],
            source_ip: TraceIp::V4(Ipv4Addr::new(203, 0, 113, 9)),
            source_port: 44321,
            event_id: Some([2; 32]),
            recipient: Some([3; 32]),
            owner: None,
            size_bytes: 512,
            correlation_id: None,
        }
    }

    #[test]
    fn network_record_is_exact_and_round_trips() {
        let record = publish();
        let encoded = record.encode().unwrap();
        assert_eq!(encoded.len(), TRACE_RECORD_LEN);
        assert_eq!(TraceRecord::decode(&encoded).unwrap(), record);
        assert!(TraceRecord::decode(&encoded[..encoded.len() - 1]).is_err());
        let mut extended = encoded.to_vec();
        extended.push(0);
        assert!(TraceRecord::decode(&extended).is_err());
    }

    #[test]
    fn rejects_noncanonical_absent_and_operation_fields() {
        let mut encoded = publish().encode().unwrap();
        encoded[62] &= !FLAG_RECIPIENT;
        assert!(TraceRecord::decode(&encoded).is_err());
        let mut record = publish();
        record.owner = Some([9; 32]);
        assert!(record.encode().is_err());
    }

    #[test]
    fn v4_padding_and_debug_are_private() {
        let mut encoded = publish().encode().unwrap();
        encoded[44] = 1;
        assert!(TraceRecord::decode(&encoded).is_err());
        let debug = format!("{:?}", publish());
        assert!(!debug.contains("203.0.113.9"));
        assert!(!debug.contains("44321"));
        assert!(!debug.contains("[1, 1"));
        assert!(!debug.contains("[2, 2"));
        assert!(!debug.contains("[3, 3"));
    }

    #[test]
    fn identity_record_is_separate_strict_and_private() {
        let record = IdentityTraceRecord {
            jurisdiction: Jurisdiction::Eu,
            timestamp_ms: 10,
            node_id: [4; 32],
            correlation_id: [5; 32],
            provider: IdentityProvider::Oidc,
            provider_subject: "person@example.invalid".into(),
        };
        let encoded = record.encode().unwrap();
        assert_eq!(encoded.len(), IDENTITY_TRACE_RECORD_LEN);
        assert_eq!(IdentityTraceRecord::decode(&encoded).unwrap(), record);
        let debug = format!("{record:?}");
        assert!(!debug.contains("person@example.invalid"));
        assert!(!debug.contains("[4, 4"));
        assert!(!debug.contains("[5, 5"));
        let mut noncanonical = encoded;
        noncanonical[204] = 1;
        assert!(IdentityTraceRecord::decode(&noncanonical).is_err());
    }
}
