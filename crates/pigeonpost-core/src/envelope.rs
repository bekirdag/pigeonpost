//! Versioned Pigeonpost envelopes, following the NIP-59 gift-wrap *pattern*.
//!
//! Three layers:
//!
//! ```text
//!   rumor  unsigned message                     what the recipient ends up reading
//!   seal   encrypted to recipient,              proves who sent it, to the recipient only
//!          signed by the SENDER
//!   wrap   encrypted to recipient,              what a loft stores; carries no link to the sender
//!          signed by a FRESH EPHEMERAL key
//! ```
//!
//! A loft sees only the wrap: a blob addressed to a pubkey, signed by a key that exists for one
//! message and is never used again. Not the sender, not the real time, not the content.
//!
//! This is not NIP-44 on the wire. Real NIP-44 is secp256k1 ECDH authenticated with HMAC-SHA256;
//! Pigeonpost identities are Ed25519, so wire compatibility was never available. The envelope is
//! X25519 ECDH → HKDF-SHA256 → XChaCha20-Poly1305. XChaCha's 192-bit nonce is chosen so nonces
//! can be random without a birthday-bound worry, since there is no shared counter between agents
//! that wake weeks apart.
//!
//! Writers emit v3. Readers retain the exact v2 signature, id, and AEAD rules so stored messages
//! remain readable. V1 was never a supported deployed format and is rejected.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use ed25519_dalek::{Signature, VerifyingKey};
use hkdf::Hkdf;
use pigeonpost_compliance_format::{
    attribution_aad, attribution_epoch_contains, attribution_epoch_end_ms, attribution_hkdf_salt,
    attribution_signing_preimage, validate_attribution_epoch, AttributionClaim, ComplianceKeyId,
    CompliancePurpose, ATTRIBUTION_BLOCK_VERSION, ATTRIBUTION_CIPHERTEXT_LEN,
    ATTRIBUTION_HKDF_INFO,
};
use rand_core::{OsRng, RngCore};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};
use crate::keys::{self, Identity};
use crate::untrusted::UntrustedBody;

/// Wire format emitted by every constructor.
pub const ENVELOPE_VERSION: u8 = 3;
/// The only legacy wire format accepted by readers.
pub const LEGACY_ENVELOPE_VERSION: u8 = 2;

const HKDF_INFO_SEAL: &[u8] = b"pigeonpost/envelope/v1/seal";
const HKDF_INFO_WRAP: &[u8] = b"pigeonpost/envelope/v1/wrap";
const SIG_DOMAIN_SEAL: &[u8] = b"pigeonpost/envelope/v1/seal-sig";
const SIG_DOMAIN_WRAP_V2: &[u8] = b"pigeonpost/envelope/v1/wrap-sig";
const SIG_DOMAIN_WRAP_V3: &[u8] = b"pigeonpost/envelope/v3/wrap-sig";
const ATTR_BIND_DOMAIN: &[u8] = b"pigeonpost/attribution-bind/v1";

/// V3 attribution ciphertexts have one exact size: 104-byte claim plus 16-byte AEAD tag.
pub const ATTR_CT_MIN: usize = ATTRIBUTION_CIPHERTEXT_LEN;
pub const ATTR_CT_MAX: usize = ATTRIBUTION_CIPHERTEXT_LEN;

/// Ciphertext is padded to a multiple of this before encryption, so that stored size leaks only
/// a coarse bucket rather than an exact message length.
const PAD_BLOCK: usize = 256;

/// Largest plaintext this version accepts. Matches the loft's default `max_event_bytes`
/// contract (`node.md`) after bounded JSON and encryption framing. Attachments are deliberately
/// out of scope: inline payload size is the one variable that multiplies every capacity number in
/// `capacity.md`.
pub const MAX_PLAINTEXT: usize = 64 * 1024;

/// Defensive decode ceiling for the outer binary ciphertext before compact JSON encoding.
///
/// A maximally escaped 64 KiB rumor, the seal, padding, and AEAD tags fit below this bound. Keeping
/// it independent of an HTTP body limit also protects direct library callers from oversized JSON
/// arrays or hex strings.
const MAX_ENVELOPE_CIPHERTEXT: usize = MAX_PLAINTEXT * 16;

/// Compact, bounded JSON representation for variable-length ciphertexts.
///
/// Writers use lowercase hex so ciphertext takes exactly two JSON bytes per binary byte instead of
/// the up-to-fourfold decimal-array expansion. Readers also accept the deployed v2 array shape.
mod compact_bytes {
    use std::fmt;

    use serde::de::{SeqAccess, Visitor};
    use serde::{Deserializer, Serialize, Serializer};

    use super::MAX_ENVELOPE_CIPHERTEXT;

    pub(super) struct Compact<'a>(pub(super) &'a [u8]);

    impl Serialize for Compact<'_> {
        fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serialize_slice(self.0, serializer)
        }
    }

    pub(super) fn serialize<S>(bytes: &[u8], serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_slice(bytes, serializer)
    }

    fn serialize_slice<S>(bytes: &[u8], serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if bytes.len() > MAX_ENVELOPE_CIPHERTEXT {
            return Err(serde::ser::Error::custom(
                "ciphertext length is outside protocol bounds",
            ));
        }
        let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        serializer.serialize_str(&encoded)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> core::result::Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor;

        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = Vec<u8>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bounded lowercase hexadecimal or a legacy byte array")
            }

            fn visit_str<E>(self, value: &str) -> core::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() > MAX_ENVELOPE_CIPHERTEXT.saturating_mul(2)
                    || !value.len().is_multiple_of(2)
                {
                    return Err(E::custom("ciphertext length is outside protocol bounds"));
                }
                let mut decoded = Vec::with_capacity(value.len() / 2);
                for pair in value.as_bytes().chunks_exact(2) {
                    let high =
                        nibble(pair[0]).ok_or_else(|| E::custom("invalid ciphertext hex"))?;
                    let low = nibble(pair[1]).ok_or_else(|| E::custom("invalid ciphertext hex"))?;
                    decoded.push((high << 4) | low);
                }
                Ok(decoded)
            }

            fn visit_seq<A>(self, mut sequence: A) -> core::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let hinted = sequence.size_hint().unwrap_or(0);
                if hinted > MAX_ENVELOPE_CIPHERTEXT {
                    return Err(serde::de::Error::custom(
                        "ciphertext length is outside protocol bounds",
                    ));
                }
                let mut decoded = Vec::with_capacity(hinted.min(MAX_ENVELOPE_CIPHERTEXT));
                while let Some(byte) = sequence.next_element::<u8>()? {
                    if decoded.len() == MAX_ENVELOPE_CIPHERTEXT {
                        return Err(serde::de::Error::custom(
                            "ciphertext length is outside protocol bounds",
                        ));
                    }
                    decoded.push(byte);
                }
                Ok(decoded)
            }
        }

        deserializer.deserialize_any(BytesVisitor)
    }

    pub(super) fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct FixedBytes<const N: usize>([u8; N]);

impl<const N: usize> Serialize for FixedBytes<N> {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        compact_bytes::serialize(&self.0, serializer)
    }
}

impl<'de, const N: usize> Deserialize<'de> for FixedBytes<N> {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FixedBytesVisitor<const N: usize>;

        impl<'de, const N: usize> serde::de::Visitor<'de> for FixedBytesVisitor<N> {
            type Value = FixedBytes<N>;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(
                    formatter,
                    "exactly {N} bytes as lowercase hexadecimal or a legacy byte array"
                )
            }

            fn visit_str<E>(self, value: &str) -> core::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let expected_len = N.checked_mul(2).ok_or_else(|| {
                    E::custom("fixed byte field length is outside protocol bounds")
                })?;
                if value.len() != expected_len {
                    return Err(E::custom("fixed byte field has the wrong length"));
                }

                let mut decoded = [0u8; N];
                for (slot, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
                    let high = compact_bytes::nibble(pair[0])
                        .ok_or_else(|| E::custom("invalid fixed byte field hex"))?;
                    let low = compact_bytes::nibble(pair[1])
                        .ok_or_else(|| E::custom("invalid fixed byte field hex"))?;
                    *slot = (high << 4) | low;
                }
                Ok(FixedBytes(decoded))
            }

            fn visit_seq<A>(self, mut sequence: A) -> core::result::Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                if sequence.size_hint().is_some_and(|hinted| hinted > N) {
                    return Err(serde::de::Error::custom(
                        "fixed byte field has the wrong length",
                    ));
                }

                let mut decoded = [0u8; N];
                for slot in &mut decoded {
                    *slot = sequence.next_element::<u8>()?.ok_or_else(|| {
                        serde::de::Error::custom("fixed byte field has the wrong length")
                    })?;
                }
                if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom(
                        "fixed byte field has the wrong length",
                    ));
                }
                Ok(FixedBytes(decoded))
            }
        }

        deserializer.deserialize_any(FixedBytesVisitor::<N>)
    }
}

/// Timestamps are jittered backwards by up to this much, so a loft cannot correlate stored
/// events by arrival time.
pub const MAX_TIMESTAMP_JITTER_SECS: u64 = 2 * 24 * 60 * 60;

/// Extra tolerance above the maximum backwards jitter for clocks that are slightly ahead.
///
/// Five minutes is intentionally much smaller than the two-day privacy jitter. It accommodates
/// ordinary clock drift without making a stale or post-dated attribution assertion useful.
pub const MAX_ATTRIBUTION_CLOCK_SKEW_MS: u64 = 5 * 60 * 1_000;

/// The message itself. Unsigned by design — it is signed once inside the seal, where only the
/// recipient can see the signature, so nobody else can prove who wrote it.
#[derive(Clone, PartialEq, Eq)]
pub struct Rumor {
    pub from: [u8; 32],
    pub to: [u8; 32],
    pub created_at: u64,
    pub body: String,
}

impl Serialize for Rumor {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("Rumor", 4)?;
        state.serialize_field("from", &FixedBytes(self.from))?;
        state.serialize_field("to", &FixedBytes(self.to))?;
        state.serialize_field("created_at", &self.created_at)?;
        state.serialize_field("body", &self.body)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Rumor {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRumor {
            from: FixedBytes<32>,
            to: FixedBytes<32>,
            created_at: u64,
            body: String,
        }

        let wire = WireRumor::deserialize(deserializer)?;
        Ok(Self {
            from: wire.from.0,
            to: wire.to.0,
            created_at: wire.created_at,
            body: wire.body,
        })
    }
}

impl core::fmt::Debug for Rumor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Rumor(<withheld>)")
    }
}

/// Sender attribution, escrowed to a published compliance key (`docs/law.md` §3).
///
/// Readable only by whoever holds the epoch's private key, under the disclosure process in
/// `law.md` §4 — never by the loft, and never by us without an order. The recipient can *verify*
/// it without being able to read anything it did not already know from the seal.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttributionBlockV2 {
    pub epoch_id: u32,
    pub e_pk: [u8; 32],
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

impl core::fmt::Debug for AttributionBlockV2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AttributionBlockV2(<withheld>)")
    }
}

/// Independently verifiable attribution block emitted by v3 writers.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttributionBlockV3 {
    /// Explicit block version. Unknown versions are rejected rather than guessed.
    pub block_version: u8,
    pub key_id: ComplianceKeyId,
    /// SHA-256 of the resolved compliance public key.
    pub compliance_key_digest: [u8; 32],
    /// Ephemeral X25519 public key, fresh per message.
    pub e_pk: [u8; 32],
    pub nonce: [u8; 24],
    /// Fixed 104-byte claim plus the 16-byte XChaCha20-Poly1305 tag.
    #[serde(with = "serde_big_array::BigArray")]
    pub ciphertext: [u8; ATTRIBUTION_CIPHERTEXT_LEN],
}

impl core::fmt::Debug for AttributionBlockV3 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AttributionBlockV3(<withheld>)")
    }
}

/// Attribution block decoded from the wire.
///
/// V2 remains parseable so old wraps can be opened, but its attribution construction is not
/// independently verifiable and is therefore never reported as [`Attribution::Valid`]. The
/// custom untagged representation keeps previously serialized v2 JSON byte-for-byte shaped as it
/// was while v3 carries an explicit `block_version`.
#[derive(Clone, PartialEq, Eq)]
pub enum AttributionBlock {
    V2(AttributionBlockV2),
    V3(AttributionBlockV3),
}

impl core::fmt::Debug for AttributionBlock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::V2(_) => f.write_str("AttributionBlock::V2(<withheld>)"),
            Self::V3(_) => f.write_str("AttributionBlock::V3(<withheld>)"),
        }
    }
}

impl AttributionBlock {
    pub fn as_v3(&self) -> Option<&AttributionBlockV3> {
        match self {
            Self::V3(block) => Some(block),
            Self::V2(_) => None,
        }
    }

    pub fn ciphertext_len(&self) -> usize {
        match self {
            Self::V2(block) => block.ciphertext.len(),
            Self::V3(block) => block.ciphertext.len(),
        }
    }
}

impl Serialize for AttributionBlock {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::V2(block) => block.serialize(serializer),
            Self::V3(block) => block.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AttributionBlock {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireBlock {
            V3(AttributionBlockV3),
            V2(AttributionBlockV2),
        }

        Ok(match WireBlock::deserialize(deserializer)? {
            WireBlock::V3(block) => {
                if block.block_version != ATTRIBUTION_BLOCK_VERSION
                    || block.key_id.purpose != CompliancePurpose::Attribution
                    || block.key_id.validate().is_err()
                {
                    return Err(serde::de::Error::custom(
                        "unknown attribution version or invalid attribution key id",
                    ));
                }
                Self::V3(block)
            }
            WireBlock::V2(block) => Self::V2(block),
        })
    }
}

/// What the recipient learned about a message's attribution.
///
/// An enum rather than a bool because **absent and invalid are different facts**: a forged block is
/// an attempt to look compliant, which is a stronger adverse signal than no block at all. A policy
/// that conflates them is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    /// No block was attached.
    Absent,
    /// A block was attached and verifies against the expected compliance key and sender.
    Valid,
    /// A block was attached and does not verify.
    Invalid,
}

/// Registry-authenticated key material and validity used by policy-enforcing recipients.
///
/// The convenient [`open_attributed`] primitive accepts only a public key for compatibility with
/// existing callers. Product code should use this type so a valid cryptographic block cannot be
/// accepted under a different key id or outside the published epoch interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedAttributionKey {
    pub key_id: ComplianceKeyId,
    pub public_key: [u8; 32],
    pub not_before_ms: u64,
    pub not_after_ms: u64,
}

impl TrustedAttributionKey {
    pub fn new(
        key_id: ComplianceKeyId,
        public_key: [u8; 32],
        not_before_ms: u64,
        not_after_ms: u64,
    ) -> Result<Self> {
        if validate_attribution_epoch(&key_id, not_before_ms, not_after_ms).is_err() {
            return Err(Error::MalformedEnvelope("invalid trusted attribution key"));
        }
        Ok(Self {
            key_id,
            public_key,
            not_before_ms,
            not_after_ms,
        })
    }
}

/// What a loft stores. Every field here is visible to the loft operator.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Wrap {
    pub version: u8,
    /// Ephemeral key that signed this wrap. Fresh per message; links to nothing.
    pub ephemeral_pubkey: [u8; 32],
    /// Recipient routing key. The one thing a loft must know to deliver it.
    pub recipient: [u8; 32],
    pub nonce: [u8; 24],
    #[serde(deserialize_with = "compact_bytes::deserialize")]
    pub ciphertext: Vec<u8>,
    /// Jittered, so it is not the real send time.
    pub created_at: u64,
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
    /// Hashcash stamp (`pow.rs`). Zero when the recipient requires no work.
    #[serde(default)]
    pub pow_nonce: u64,
    /// Escrowed sender attribution. Absent wherever it is not required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AttributionBlock>,
}

impl core::fmt::Debug for Wrap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Ciphertext, routing identity, replayable proof material, and attribution are all
        // intentionally absent. This remains safe when a downstream integration logs errors.
        f.debug_struct("Wrap")
            .field("version", &self.version)
            .field("contents", &"withheld")
            .finish()
    }
}

impl Serialize for Wrap {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state =
            serializer.serialize_struct("Wrap", if self.attribution.is_some() { 9 } else { 8 })?;
        state.serialize_field("version", &self.version)?;
        state.serialize_field("ephemeral_pubkey", &self.ephemeral_pubkey)?;
        state.serialize_field("recipient", &self.recipient)?;
        state.serialize_field("nonce", &self.nonce)?;
        if self.version == LEGACY_ENVELOPE_VERSION {
            // Preserve the deployed v2 JSON array shape when a legacy stored event is fetched.
            state.serialize_field("ciphertext", &self.ciphertext)?;
        } else {
            state.serialize_field("ciphertext", &compact_bytes::Compact(&self.ciphertext))?;
        }
        state.serialize_field("created_at", &self.created_at)?;
        state.serialize_field("signature", self.signature.as_slice())?;
        state.serialize_field("pow_nonce", &self.pow_nonce)?;
        if let Some(attribution) = &self.attribution {
            state.serialize_field("attribution", attribution)?;
        }
        state.end()
    }
}

impl Wrap {
    /// Stable identifier for this event.
    ///
    /// Excluding the stamp is what makes mining possible — the miner varies `pow_nonce` while the
    /// id it is grinding against stays fixed. It also means the id is the natural dedupe key when
    /// the same message arrives from two or three lofts.
    ///
    /// V3 hashes a versioned domain and the signed outer core fields, excluding the signature,
    /// proof-of-work nonce, and attribution block. V2 retains its historical algorithm, including
    /// the signature, solely for read and dedupe compatibility.
    pub fn id(&self) -> [u8; 32] {
        if self.version == LEGACY_ENVELOPE_VERSION {
            return wrap_id_v2(self);
        }
        wrap_id_v3(
            self.version,
            &self.ephemeral_pubkey,
            &self.recipient,
            &self.nonce,
            &self.ciphertext,
            self.created_at,
        )
    }

    /// Mine a stamp meeting `difficulty`. No-op when the recipient requires no work.
    pub fn stamp(&mut self, difficulty: u32, max_attempts: u64) -> Result<()> {
        if difficulty == 0 {
            self.pow_nonce = 0;
            return Ok(());
        }
        let id = self.id();
        self.pow_nonce =
            crate::pow::mine(&id, difficulty, max_attempts).ok_or(Error::InsufficientWork)?;
        Ok(())
    }

    /// Check the stamp against a required difficulty.
    pub fn verify_work(&self, difficulty: u32) -> Result<()> {
        crate::pow::verify(&self.id(), self.pow_nonce, difficulty)
    }

    /// Validate a v3 wrap's public structure and outer signature without decrypting it.
    ///
    /// This is the loft admission primitive. It intentionally rejects legacy v2 writes while
    /// [`open`] remains able to read them. Attribution-key resolution and digest matching are
    /// caller responsibilities because they require a checkpoint-pinned registry view.
    pub fn verify_public(&self) -> Result<()> {
        if self.version != ENVELOPE_VERSION {
            return Err(Error::MalformedEnvelope("v3 required for publish"));
        }
        keys::verifying_key_from_bytes(&self.recipient)?;
        let ephemeral_pubkey = keys::verifying_key_from_bytes(&self.ephemeral_pubkey)?;
        if self.ciphertext.len() <= 16 || !(self.ciphertext.len() - 16).is_multiple_of(PAD_BLOCK) {
            return Err(Error::MalformedEnvelope("outer ciphertext length"));
        }
        if let Some(block) = &self.attribution {
            validate_public_attribution_v3(block)?;
        }
        let payload = wrap_signing_payload_v3(
            self.version,
            &self.ephemeral_pubkey,
            &self.recipient,
            self.created_at,
            &self.nonce,
            &self.ciphertext,
            self.attribution.as_ref(),
        )?;
        keys::verify(
            &ephemeral_pubkey,
            &payload,
            &Signature::from_bytes(&self.signature),
        )
    }
}

fn validate_public_attribution_v3(block: &AttributionBlock) -> Result<()> {
    let AttributionBlock::V3(block) = block else {
        return Err(Error::MalformedEnvelope("v2 attribution on v3 envelope"));
    };
    if block.block_version != ATTRIBUTION_BLOCK_VERSION
        || block.key_id.purpose != CompliancePurpose::Attribution
        || block.key_id.validate().is_err()
    {
        return Err(Error::MalformedEnvelope("attribution version or key id"));
    }
    if block.e_pk == [0u8; 32] {
        return Err(Error::InvalidKey);
    }
    if block.compliance_key_digest == [0u8; 32] {
        return Err(Error::MalformedEnvelope("empty compliance key digest"));
    }
    Ok(())
}

fn wrap_id_v2(wrapped: &Wrap) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pigeonpost/wrap-id/v1");
    hasher.update([wrapped.version]);
    hasher.update(wrapped.ephemeral_pubkey);
    hasher.update(wrapped.recipient);
    hasher.update(wrapped.nonce);
    hasher.update((wrapped.ciphertext.len() as u64).to_le_bytes());
    hasher.update(&wrapped.ciphertext);
    hasher.update(wrapped.created_at.to_le_bytes());
    hasher.update(wrapped.signature);
    hasher.finalize().into()
}

fn wrap_id_v3(
    version: u8,
    ephemeral_pubkey: &[u8; 32],
    recipient: &[u8; 32],
    nonce: &[u8; 24],
    ciphertext: &[u8],
    created_at: u64,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pigeonpost/wrap-id/v3");
    hasher.update([version]);
    hasher.update(ephemeral_pubkey);
    hasher.update(recipient);
    hasher.update(nonce);
    hasher.update((ciphertext.len() as u64).to_be_bytes());
    hasher.update(ciphertext);
    hasher.update(created_at.to_be_bytes());
    hasher.finalize().into()
}

struct Seal {
    sender_pubkey: [u8; 32],
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
    /// The attribution ephemeral secret, readable only by the recipient.
    ///
    /// Putting it here is what makes attribution *verifiable* rather than merely present: the
    /// recipient can derive the same key the compliance holder would, and check the block says
    /// what it claims — without learning anything the seal did not already tell it.
    attribution_sk: Option<[u8; 32]>,
    signature: [u8; 64],
}

impl Serialize for Seal {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer
            .serialize_struct("Seal", if self.attribution_sk.is_some() { 5 } else { 4 })?;
        state.serialize_field("sender_pubkey", &FixedBytes(self.sender_pubkey))?;
        state.serialize_field("nonce", &FixedBytes(self.nonce))?;
        state.serialize_field("ciphertext", &compact_bytes::Compact(&self.ciphertext))?;
        if let Some(attribution_sk) = self.attribution_sk {
            state.serialize_field("attribution_sk", &FixedBytes(attribution_sk))?;
        }
        state.serialize_field("signature", &FixedBytes(self.signature))?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Seal {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireSeal {
            sender_pubkey: FixedBytes<32>,
            nonce: FixedBytes<24>,
            #[serde(deserialize_with = "compact_bytes::deserialize")]
            ciphertext: Vec<u8>,
            #[serde(default)]
            attribution_sk: Option<FixedBytes<32>>,
            signature: FixedBytes<64>,
        }

        let wire = WireSeal::deserialize(deserializer)?;
        Ok(Self {
            sender_pubkey: wire.sender_pubkey.0,
            nonce: wire.nonce.0,
            ciphertext: wire.ciphertext,
            attribution_sk: wire.attribution_sk.map(|bytes| bytes.0),
            signature: wire.signature.0,
        })
    }
}

/// Seal and wrap a message for `recipient`.
///
/// `now` is the true send time; the wrap's visible timestamp is jittered backwards from it.
pub fn wrap(sender: &Identity, recipient: &VerifyingKey, body: &str, now: u64) -> Result<Wrap> {
    wrap_inner(sender, recipient, body, now, None)
}

/// Seal and wrap, attaching an escrowed attribution block (`docs/law.md` §3).
///
/// `p_c` must come from the transparency log, never from anything the sender chose — see
/// `verify_attribution`.
pub fn wrap_attributed(
    sender: &Identity,
    recipient: &VerifyingKey,
    body: &str,
    now: u64,
    p_c: &[u8; 32],
    key_id: &ComplianceKeyId,
) -> Result<Wrap> {
    let sent_at_ms = now
        .checked_mul(1_000)
        .ok_or(Error::MalformedEnvelope("invalid attribution send time"))?;
    if attribution_epoch_contains(key_id, sent_at_ms) != Ok(true) {
        return Err(Error::MalformedEnvelope("invalid attribution key id"));
    }
    wrap_inner(sender, recipient, body, now, Some((p_c, key_id)))
}

fn wrap_inner(
    sender: &Identity,
    recipient: &VerifyingKey,
    body: &str,
    now: u64,
    compliance: Option<(&[u8; 32], &ComplianceKeyId)>,
) -> Result<Wrap> {
    if body.len() > MAX_PLAINTEXT {
        return Err(Error::TooLarge);
    }

    let rumor = Rumor {
        from: sender.verifying_key().to_bytes(),
        to: recipient.to_bytes(),
        created_at: now,
        body: body.to_owned(),
    };
    let rumor_bytes = serde_json::to_vec(&rumor).map_err(|_| Error::MalformedEnvelope("rumor"))?;

    // Layer 1: seal — encrypted to the recipient, signed by the real sender.
    let seal_key = derive_key(&sender.agree(recipient)?, HKDF_INFO_SEAL);
    let seal_nonce = random_nonce();
    let seal_ct = encrypt(&seal_key, &seal_nonce, &pad(&rumor_bytes), ENVELOPE_VERSION)?;

    // Generate the attribution key before signing the seal. The block itself must wait until the
    // outer ciphertext and therefore event id exist; the seal signature binds this exact secret.
    let attribution_sk = compliance.map(|_| keys::ephemeral().to_seed());

    let seal_sig = sender.sign(&seal_signing_payload(
        &seal_nonce,
        &seal_ct,
        attribution_sk.as_ref(),
    ));

    let seal = Seal {
        sender_pubkey: sender.verifying_key().to_bytes(),
        nonce: seal_nonce,
        ciphertext: seal_ct,
        attribution_sk,
        signature: seal_sig.to_bytes(),
    };
    let seal_bytes = serde_json::to_vec(&seal).map_err(|_| Error::MalformedEnvelope("seal"))?;

    // Layer 2: wrap — encrypted to the recipient again, signed by a key used exactly once.
    let ephemeral = keys::ephemeral();
    let wrap_key = derive_key(&ephemeral.agree(recipient)?, HKDF_INFO_WRAP);
    let wrap_nonce = random_nonce();
    let wrap_ct = encrypt(&wrap_key, &wrap_nonce, &pad(&seal_bytes), ENVELOPE_VERSION)?;
    let created_at = jitter(now);
    let ephemeral_pubkey = ephemeral.verifying_key().to_bytes();
    let recipient_bytes = recipient.to_bytes();
    let event_id = wrap_id_v3(
        ENVELOPE_VERSION,
        &ephemeral_pubkey,
        &recipient_bytes,
        &wrap_nonce,
        &wrap_ct,
        created_at,
    );

    let attribution = match (compliance, attribution_sk.as_ref()) {
        (Some((p_c, key_id)), Some(e_sk)) => Some(AttributionBlock::V3(build_attribution_v3(
            sender,
            now,
            p_c,
            key_id,
            e_sk,
            &event_id,
            &recipient_bytes,
        )?)),
        (None, None) => None,
        _ => return Err(Error::MalformedEnvelope("attribution construction")),
    };

    let wrap_sig = ephemeral.sign(&wrap_signing_payload_v3(
        ENVELOPE_VERSION,
        &ephemeral_pubkey,
        &recipient_bytes,
        created_at,
        &wrap_nonce,
        &wrap_ct,
        attribution.as_ref(),
    )?);

    Ok(Wrap {
        version: ENVELOPE_VERSION,
        ephemeral_pubkey,
        recipient: recipient_bytes,
        nonce: wrap_nonce,
        ciphertext: wrap_ct,
        created_at,
        signature: wrap_sig.to_bytes(),
        pow_nonce: 0,
        attribution,
    })
}

/// Open a wrap. Returns the sender's verified key and the body, marked untrusted.
///
/// The sender is authenticated *inside* the seal, and the rumor's `from` must match the key that
/// signed it — otherwise a sender could seal a message claiming to be someone else.
pub fn open(recipient: &Identity, wrapped: &Wrap) -> Result<(VerifyingKey, UntrustedBody)> {
    let (sender, body, _) = open_attributed(recipient, wrapped, None)?;
    Ok((sender, body))
}

/// Open a wrap and judge its attribution.
///
/// `expected_p_c` is the compliance public key for the block's typed key id, **fetched from the
/// transparency log**. Passing a key the message supplied would let a sender escrow to a key nobody
/// holds — the entire failure this design closes (`docs/law.md` §3.2). `None` means the caller
/// could not resolve the epoch, which yields `Invalid` rather than `Valid`: unverifiable is not
/// verified.
pub fn open_attributed(
    recipient: &Identity,
    wrapped: &Wrap,
    expected_p_c: Option<&[u8; 32]>,
) -> Result<(VerifyingKey, UntrustedBody, Attribution)> {
    open_attributed_inner(recipient, wrapped, expected_p_c, None)
}

/// Open a wrap and enforce the exact registry key id and validity interval.
pub fn open_attributed_trusted(
    recipient: &Identity,
    wrapped: &Wrap,
    trusted: Option<&TrustedAttributionKey>,
) -> Result<(VerifyingKey, UntrustedBody, Attribution)> {
    open_attributed_inner(
        recipient,
        wrapped,
        trusted.map(|key| &key.public_key),
        trusted,
    )
}

fn open_attributed_inner(
    recipient: &Identity,
    wrapped: &Wrap,
    expected_p_c: Option<&[u8; 32]>,
    trusted: Option<&TrustedAttributionKey>,
) -> Result<(VerifyingKey, UntrustedBody, Attribution)> {
    if !matches!(wrapped.version, LEGACY_ENVELOPE_VERSION | ENVELOPE_VERSION) {
        return Err(Error::MalformedEnvelope("unsupported version"));
    }
    if wrapped.recipient != recipient.verifying_key().to_bytes() {
        return Err(Error::MalformedEnvelope("not addressed to this key"));
    }

    let ephemeral_pubkey = keys::verifying_key_from_bytes(&wrapped.ephemeral_pubkey)?;
    let wrap_sig = Signature::from_bytes(&wrapped.signature);
    let signing_payload = if wrapped.version == LEGACY_ENVELOPE_VERSION {
        wrap_signing_payload_v2(
            wrapped.version,
            &wrapped.recipient,
            wrapped.created_at,
            &wrapped.nonce,
            &wrapped.ciphertext,
            wrapped.attribution.as_ref(),
        )
    } else {
        wrap_signing_payload_v3(
            wrapped.version,
            &wrapped.ephemeral_pubkey,
            &wrapped.recipient,
            wrapped.created_at,
            &wrapped.nonce,
            &wrapped.ciphertext,
            wrapped.attribution.as_ref(),
        )?
    };
    keys::verify(&ephemeral_pubkey, &signing_payload, &wrap_sig)?;

    let wrap_key = derive_key(&recipient.agree(&ephemeral_pubkey)?, HKDF_INFO_WRAP);
    let seal_bytes = unpad(&decrypt(
        &wrap_key,
        &wrapped.nonce,
        &wrapped.ciphertext,
        wrapped.version,
    )?);
    let seal: Seal =
        serde_json::from_slice(&seal_bytes).map_err(|_| Error::MalformedEnvelope("seal"))?;

    let sender_pubkey = keys::verifying_key_from_bytes(&seal.sender_pubkey)?;
    let seal_sig = Signature::from_bytes(&seal.signature);
    keys::verify(
        &sender_pubkey,
        &seal_signing_payload(&seal.nonce, &seal.ciphertext, seal.attribution_sk.as_ref()),
        &seal_sig,
    )?;

    let seal_key = derive_key(&recipient.agree(&sender_pubkey)?, HKDF_INFO_SEAL);
    let rumor_bytes = unpad(&decrypt(
        &seal_key,
        &seal.nonce,
        &seal.ciphertext,
        wrapped.version,
    )?);
    let rumor: Rumor =
        serde_json::from_slice(&rumor_bytes).map_err(|_| Error::MalformedEnvelope("rumor"))?;

    if rumor.from != seal.sender_pubkey {
        return Err(Error::SenderMismatch);
    }
    if rumor.to != recipient.verifying_key().to_bytes() {
        return Err(Error::MalformedEnvelope("rumor addressed elsewhere"));
    }

    let attribution = match (&wrapped.attribution, &seal.attribution_sk, wrapped.version) {
        (None, _, _) => Attribution::Absent,
        // V2 attribution was recipient-correlated but not independently custodian-verifiable. It
        // remains readable but must never be represented as compliance-valid.
        (Some(_), _, LEGACY_ENVELOPE_VERSION) => Attribution::Invalid,
        (Some(block), Some(e_sk), ENVELOPE_VERSION) => {
            match expected_p_c {
                Some(p_c)
                    if verify_attribution_claim(
                        block,
                        e_sk,
                        p_c,
                        &wrapped.id(),
                        &wrapped.recipient,
                        wrapped.created_at,
                        &sender_pubkey,
                    )
                    .is_ok_and(|claim| {
                        trusted.is_none_or(|key| {
                            block
                                .as_v3()
                                .is_some_and(|block| block.key_id == key.key_id)
                                && key.not_before_ms <= claim.sent_at_ms
                                && claim.sent_at_ms < key.not_after_ms
                        })
                    }) =>
                {
                    Attribution::Valid
                }
                // Either it failed, or we could not resolve the epoch key. Both are "not verified",
                // and neither may be reported as valid.
                _ => Attribution::Invalid,
            }
        }
        // A block with no secret to check it against cannot be verified by anyone but the
        // compliance holder, so the recipient must not treat it as proof of anything.
        (Some(_), None, ENVELOPE_VERSION) => Attribution::Invalid,
        (Some(_), _, _) => Attribution::Invalid,
    };

    Ok((sender_pubkey, UntrustedBody::new(rumor.body), attribution))
}

/// Build a block escrowed to `p_c`, returning it with the ephemeral secret for the seal.
fn build_attribution_v3(
    sender: &Identity,
    now: u64,
    p_c: &[u8; 32],
    key_id: &ComplianceKeyId,
    e_sk: &[u8; 32],
    event_id: &[u8; 32],
    recipient: &[u8; 32],
) -> Result<AttributionBlockV3> {
    let ephemeral = Identity::from_seed(*e_sk);
    let e_pk = keys::x25519_public(&ephemeral);
    let compliance_key_digest: [u8; 32] = Sha256::digest(p_c).into();

    let shared = keys::x25519_agree(&ephemeral, p_c)?;
    let k_attr = derive_attr_key_v3(&shared, &e_pk, &compliance_key_digest);
    let sent_at_ms = now.saturating_mul(1_000);
    let preimage = attribution_signing_preimage(
        ATTRIBUTION_BLOCK_VERSION,
        key_id,
        &compliance_key_digest,
        &e_pk,
        event_id,
        recipient,
        sent_at_ms,
    )
    .map_err(|_| Error::MalformedEnvelope("attribution context"))?;
    let claim = AttributionClaim {
        sender_pubkey: sender.verifying_key().to_bytes(),
        sent_at_ms,
        signature: sender.sign(&preimage).to_bytes(),
    };
    let plain = claim.encode();

    let nonce = random_nonce();
    let ciphertext: [u8; ATTRIBUTION_CIPHERTEXT_LEN] = XChaCha20Poly1305::new(&k_attr.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: &attribution_aad(
                    ATTRIBUTION_BLOCK_VERSION,
                    key_id,
                    &compliance_key_digest,
                    &e_pk,
                    event_id,
                    recipient,
                )
                .map_err(|_| Error::MalformedEnvelope("attribution context"))?,
            },
        )
        .map_err(|_| Error::DecryptionFailed)?
        .try_into()
        .map_err(|_| Error::MalformedEnvelope("attribution ciphertext length"))?;

    Ok(AttributionBlockV3 {
        block_version: ATTRIBUTION_BLOCK_VERSION,
        key_id: *key_id,
        compliance_key_digest,
        e_pk,
        nonce,
        ciphertext,
    })
}

/// Check that a block escrows *this* sender, on *this* message, to *this* compliance key.
///
/// The recipient can run this because the seal carries `e_sk`; it learns nothing new, since the
/// seal already told it who sent the message. What it gains is the ability to disbelieve a sender
/// that attached a block escrowed to a key nobody holds.
pub fn verify_attribution(
    block: &AttributionBlock,
    e_sk: &[u8; 32],
    p_c_expected: &[u8; 32],
    event_id: &[u8; 32],
    recipient: &[u8; 32],
    created_at: u64,
    expected_sender: &VerifyingKey,
) -> Result<()> {
    verify_attribution_claim(
        block,
        e_sk,
        p_c_expected,
        event_id,
        recipient,
        created_at,
        expected_sender,
    )
    .map(|_| ())
}

fn verify_attribution_claim(
    block: &AttributionBlock,
    e_sk: &[u8; 32],
    p_c_expected: &[u8; 32],
    event_id: &[u8; 32],
    recipient: &[u8; 32],
    created_at: u64,
    expected_sender: &VerifyingKey,
) -> Result<AttributionClaim> {
    let AttributionBlock::V3(block) = block else {
        return Err(Error::MalformedEnvelope(
            "legacy attribution is not compliance-valid",
        ));
    };
    // `created_at` is deliberately jittered backwards and may cross a month boundary. The
    // recipient must validate the true sender-signed `claim.sent_at_ms` against the monthly key;
    // the visible time only bounds that claim below. Still reject a malformed/non-attribution
    // calendar key before doing any cryptographic work.
    if block.block_version != ATTRIBUTION_BLOCK_VERSION
        || attribution_epoch_end_ms(&block.key_id).is_err()
    {
        return Err(Error::MalformedEnvelope("attribution version or key id"));
    }
    let ephemeral = Identity::from_seed(*e_sk);

    // The block names its own `e_pk`; recomputing it from the secret is what stops a block being
    // lifted from another message and stapled here with its original `e_pk` intact.
    if keys::x25519_public(&ephemeral) != block.e_pk {
        return Err(Error::MalformedEnvelope("attribution key mismatch"));
    }

    let expected_digest: [u8; 32] = Sha256::digest(p_c_expected).into();
    if !bool::from(block.compliance_key_digest.ct_eq(&expected_digest)) {
        return Err(Error::MalformedEnvelope("compliance key digest mismatch"));
    }

    let shared = keys::x25519_agree(&ephemeral, p_c_expected)?;
    let k_attr = derive_attr_key_v3(&shared, &block.e_pk, &block.compliance_key_digest);

    let plain = XChaCha20Poly1305::new(&k_attr.into())
        .decrypt(
            XNonce::from_slice(&block.nonce),
            Payload {
                msg: &block.ciphertext,
                aad: &attribution_aad(
                    block.block_version,
                    &block.key_id,
                    &block.compliance_key_digest,
                    &block.e_pk,
                    event_id,
                    recipient,
                )
                .map_err(|_| Error::MalformedEnvelope("attribution context"))?,
            },
        )
        .map_err(|_| Error::DecryptionFailed)?;

    let claim = AttributionClaim::decode(&plain)
        .map_err(|_| Error::MalformedEnvelope("attribution claim"))?;

    if claim.sender_pubkey != expected_sender.to_bytes() {
        return Err(Error::SenderMismatch);
    }
    let claimed_sender = keys::verifying_key_from_bytes(&claim.sender_pubkey)?;
    let preimage = attribution_signing_preimage(
        block.block_version,
        &block.key_id,
        &block.compliance_key_digest,
        &block.e_pk,
        event_id,
        recipient,
        claim.sent_at_ms,
    )
    .map_err(|_| Error::MalformedEnvelope("attribution context"))?;
    keys::verify(
        &claimed_sender,
        &preimage,
        &Signature::from_bytes(&claim.signature),
    )?;

    let created_at_ms = created_at.checked_mul(1_000).ok_or(Error::StaleTimestamp)?;
    let latest = created_at_ms
        .saturating_add(MAX_TIMESTAMP_JITTER_SECS.saturating_mul(1_000))
        .saturating_add(MAX_ATTRIBUTION_CLOCK_SKEW_MS);
    if claim.sent_at_ms < created_at_ms || claim.sent_at_ms > latest {
        return Err(Error::StaleTimestamp);
    }
    if attribution_epoch_contains(&block.key_id, claim.sent_at_ms) != Ok(true) {
        return Err(Error::StaleTimestamp);
    }
    Ok(claim)
}

fn derive_attr_key_v3(
    shared: &[u8; 32],
    e_pk: &[u8; 32],
    compliance_key_digest: &[u8; 32],
) -> [u8; 32] {
    let salt = attribution_hkdf_salt(e_pk, compliance_key_digest);
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut key = [0u8; 32];
    hkdf.expand(ATTRIBUTION_HKDF_INFO, &mut key)
        .expect("32 bytes is a valid HKDF output length");
    key
}

/// Legacy v2 seal binding, retained as a public primitive for readers and diagnostic tooling.
///
/// The seal ciphertext, not the wrap id: the block lives inside the wrap, so the id cannot be known
/// when the block is built. The seal is fixed before the wrap is assembled, unique per message, and
/// already signed by the real sender — so the recipient has authenticated it before it checks
/// attribution (`docs/law.md` §3.2).
pub fn seal_bind(seal_nonce: &[u8; 24], seal_ct: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ATTR_BIND_DOMAIN);
    hasher.update(seal_nonce);
    hasher.update((seal_ct.len() as u64).to_le_bytes());
    hasher.update(seal_ct);
    hasher.finalize().into()
}

/// Seal signature payload. Covers `attribution_sk`, so a sender cannot be handed an ephemeral
/// secret it never authorised.
fn seal_signing_payload(nonce: &[u8; 24], ciphertext: &[u8], sk: Option<&[u8; 32]>) -> Vec<u8> {
    let mut payload = signing_payload(SIG_DOMAIN_SEAL, nonce, ciphertext);
    match sk {
        Some(sk) => {
            payload.push(1);
            payload.extend_from_slice(sk);
        }
        None => payload.push(0),
    }
    payload
}

/// Wrap signature payload.
///
/// Covers **every field that feeds [`Wrap::id`]** except the signature itself. Anything inside the
/// id but outside the signature would let a hostile loft mutate a message into a different event
/// id — breaking cross-loft dedupe, and breaking the trace-log correlation that keys on
/// `event_id`. Covering the attribution block also means stripping one in transit fails the
/// signature rather than silently downgrading the message to unattributed.
fn wrap_signing_payload_v2(
    version: u8,
    recipient: &[u8; 32],
    created_at: u64,
    nonce: &[u8; 24],
    ciphertext: &[u8],
    attribution: Option<&AttributionBlock>,
) -> Vec<u8> {
    let mut payload = signing_payload(SIG_DOMAIN_WRAP_V2, nonce, ciphertext);
    payload.push(version);
    payload.extend_from_slice(recipient);
    payload.extend_from_slice(&created_at.to_le_bytes());
    match attribution {
        Some(AttributionBlock::V2(block)) => {
            payload.push(1);
            payload.extend_from_slice(&block.epoch_id.to_le_bytes());
            payload.extend_from_slice(&block.e_pk);
            payload.extend_from_slice(&block.nonce);
            payload.extend_from_slice(&(block.ciphertext.len() as u64).to_le_bytes());
            payload.extend_from_slice(&block.ciphertext);
        }
        Some(AttributionBlock::V3(block)) => {
            payload.push(1);
            payload.push(block.block_version);
            match block.key_id.encode() {
                Ok(encoded) => payload.extend_from_slice(&encoded),
                Err(_) => payload
                    .extend_from_slice(&[0u8; pigeonpost_compliance_format::COMPLIANCE_KEY_ID_LEN]),
            }
            payload.extend_from_slice(&block.compliance_key_digest);
            payload.extend_from_slice(&block.e_pk);
            payload.extend_from_slice(&block.nonce);
            payload.extend_from_slice(&block.ciphertext);
        }
        None => payload.push(0),
    }
    payload
}

fn wrap_signing_payload_v3(
    version: u8,
    ephemeral_pubkey: &[u8; 32],
    recipient: &[u8; 32],
    created_at: u64,
    nonce: &[u8; 24],
    ciphertext: &[u8],
    attribution: Option<&AttributionBlock>,
) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(
        SIG_DOMAIN_WRAP_V3.len()
            + 1
            + 32
            + 32
            + 24
            + 8
            + ciphertext.len()
            + 8
            + 1
            + 1
            + pigeonpost_compliance_format::COMPLIANCE_KEY_ID_LEN
            + 32
            + 32
            + 24
            + ATTRIBUTION_CIPHERTEXT_LEN,
    );
    payload.extend_from_slice(SIG_DOMAIN_WRAP_V3);
    payload.push(version);
    payload.extend_from_slice(ephemeral_pubkey);
    payload.extend_from_slice(recipient);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&(ciphertext.len() as u64).to_be_bytes());
    payload.extend_from_slice(ciphertext);
    payload.extend_from_slice(&created_at.to_be_bytes());
    match attribution {
        Some(AttributionBlock::V3(block)) => {
            payload.push(1);
            payload.push(block.block_version);
            payload.extend_from_slice(
                &block
                    .key_id
                    .encode()
                    .map_err(|_| Error::MalformedEnvelope("attribution key id"))?,
            );
            payload.extend_from_slice(&block.compliance_key_digest);
            payload.extend_from_slice(&block.e_pk);
            payload.extend_from_slice(&block.nonce);
            payload.extend_from_slice(&block.ciphertext);
        }
        Some(AttributionBlock::V2(_)) => {
            return Err(Error::MalformedEnvelope("v2 attribution on v3 envelope"));
        }
        None => payload.push(0),
    }
    Ok(payload)
}

fn derive_key(shared_secret: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let hkdf = Hkdf::<Sha256>::new(None, shared_secret);
    let mut key = [0u8; 32];
    hkdf.expand(info, &mut key)
        .expect("32 bytes is a valid HKDF output length");
    key
}

fn encrypt(key: &[u8; 32], nonce: &[u8; 24], plaintext: &[u8], version: u8) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: &[version],
            },
        )
        .map_err(|_| Error::DecryptionFailed)
}

fn decrypt(key: &[u8; 32], nonce: &[u8; 24], ciphertext: &[u8], version: u8) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &[version],
            },
        )
        .map_err(|_| Error::DecryptionFailed)
}

/// Length-prefix then pad to a `PAD_BLOCK` multiple.
fn pad(plaintext: &[u8]) -> Vec<u8> {
    let total = (plaintext.len() + 4).div_ceil(PAD_BLOCK) * PAD_BLOCK;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    out.extend_from_slice(plaintext);
    out.resize(total, 0);
    out
}

fn unpad(padded: &[u8]) -> Vec<u8> {
    if padded.len() < 4 {
        return Vec::new();
    }
    let len = u32::from_le_bytes([padded[0], padded[1], padded[2], padded[3]]) as usize;
    let end = 4usize.saturating_add(len);
    if end > padded.len() {
        return Vec::new();
    }
    padded[4..end].to_vec()
}

fn signing_payload(domain: &[u8], nonce: &[u8; 24], ciphertext: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(domain.len() + 24 + ciphertext.len());
    payload.extend_from_slice(domain);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(ciphertext);
    payload
}

fn random_nonce() -> [u8; 24] {
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

fn jitter(now: u64) -> u64 {
    let mut buf = [0u8; 8];
    OsRng.fill_bytes(&mut buf);
    let offset = u64::from_le_bytes(buf) % (MAX_TIMESTAMP_JITTER_SECS + 1);
    now.saturating_sub(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_786_105_721;

    fn pair() -> (Identity, Identity) {
        (Identity::from_seed([11; 32]), Identity::from_seed([22; 32]))
    }

    #[test]
    fn round_trips() {
        let (alice, bob) = pair();
        let wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
        let (sender, body) = open(&bob, &wrapped).unwrap();

        assert_eq!(sender, alice.verifying_key());
        assert_eq!(body.as_str(), "hello");
    }

    #[test]
    fn a_third_party_cannot_open_it() {
        let (alice, bob) = pair();
        let eve = Identity::from_seed([33; 32]);
        let wrapped = wrap(&alice, &bob.verifying_key(), "secret", NOW).unwrap();

        assert!(open(&eve, &wrapped).is_err());
    }

    #[test]
    fn the_wrap_reveals_nothing_about_the_sender() {
        let (alice, bob) = pair();
        let wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();

        assert_ne!(wrapped.ephemeral_pubkey, alice.verifying_key().to_bytes());
        let serialized = serde_json::to_vec(&wrapped).unwrap();
        assert!(
            !serialized
                .windows(32)
                .any(|w| w == alice.verifying_key().as_bytes()),
            "the sender's key must not appear anywhere a loft can see"
        );
    }

    #[test]
    fn each_message_uses_a_fresh_ephemeral_key() {
        let (alice, bob) = pair();
        let first = wrap(&alice, &bob.verifying_key(), "one", NOW).unwrap();
        let second = wrap(&alice, &bob.verifying_key(), "two", NOW).unwrap();

        assert_ne!(
            first.ephemeral_pubkey, second.ephemeral_pubkey,
            "a reused wrapper key would correlate two messages as one conversation"
        );
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let (alice, bob) = pair();
        let mut wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
        wrapped.ciphertext[0] ^= 0xff;

        assert_eq!(open(&bob, &wrapped), Err(Error::BadSignature));
    }

    #[test]
    fn re_signing_tampered_ciphertext_still_fails_to_decrypt() {
        let (alice, bob) = pair();
        let mut wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();

        // An attacker who re-signs with their own ephemeral key gets past signature checking,
        // and then hits the AEAD.
        let attacker = Identity::from_seed([44; 32]);
        wrapped.ciphertext[0] ^= 0xff;
        wrapped.ephemeral_pubkey = attacker.verifying_key().to_bytes();
        wrapped.signature = attacker
            .sign(
                &wrap_signing_payload_v3(
                    wrapped.version,
                    &wrapped.ephemeral_pubkey,
                    &wrapped.recipient,
                    wrapped.created_at,
                    &wrapped.nonce,
                    &wrapped.ciphertext,
                    wrapped.attribution.as_ref(),
                )
                .unwrap(),
            )
            .to_bytes();

        assert_eq!(open(&bob, &wrapped), Err(Error::DecryptionFailed));
    }

    #[test]
    fn message_for_someone_else_is_rejected() {
        let (alice, bob) = pair();
        let carol = Identity::from_seed([55; 32]);
        let wrapped = wrap(&alice, &carol.verifying_key(), "for carol", NOW).unwrap();

        assert_eq!(
            open(&bob, &wrapped),
            Err(Error::MalformedEnvelope("not addressed to this key"))
        );
    }

    #[test]
    fn timestamp_is_jittered_backwards_only() {
        let (alice, bob) = pair();
        for _ in 0..50 {
            let wrapped = wrap(&alice, &bob.verifying_key(), "hi", NOW).unwrap();
            assert!(wrapped.created_at <= NOW);
            assert!(wrapped.created_at >= NOW - MAX_TIMESTAMP_JITTER_SECS);
        }
    }

    #[test]
    fn padding_hides_exact_length() {
        let (alice, bob) = pair();
        let short = wrap(&alice, &bob.verifying_key(), "a", NOW).unwrap();
        let longer = wrap(&alice, &bob.verifying_key(), "abcdefghij", NOW).unwrap();
        let other_sender = Identity::from_seed([33; 32]);
        let other = wrap(&other_sender, &bob.verifying_key(), "a", NOW).unwrap();

        assert_eq!(
            short.ciphertext.len(),
            longer.ciphertext.len(),
            "messages in the same bucket must be indistinguishable by size"
        );
        assert_eq!(
            short.ciphertext.len(),
            other.ciphertext.len(),
            "fixed inner fields must not make ciphertext size sender-dependent"
        );
    }

    #[test]
    fn fixed_inner_fields_write_hex_and_read_legacy_arrays() {
        let seal = Seal {
            sender_pubkey: [0x12; 32],
            nonce: [0x34; 24],
            ciphertext: vec![0x56; 32],
            attribution_sk: Some([0x78; 32]),
            signature: [0x9a; 64],
        };
        let encoded = serde_json::to_value(&seal).unwrap();
        for (field, bytes) in [
            ("sender_pubkey", 32usize),
            ("nonce", 24),
            ("attribution_sk", 32),
            ("signature", 64),
        ] {
            let value = encoded[field].as_str().unwrap();
            assert_eq!(value.len(), bytes * 2);
            assert!(value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        }

        let round_trip: Seal = serde_json::from_value(encoded).unwrap();
        assert_eq!(round_trip.sender_pubkey, seal.sender_pubkey);
        assert_eq!(round_trip.nonce, seal.nonce);
        assert_eq!(round_trip.ciphertext, seal.ciphertext);
        assert_eq!(round_trip.attribution_sk, seal.attribution_sk);
        assert_eq!(round_trip.signature, seal.signature);

        let legacy = serde_json::json!({
            "sender_pubkey": vec![0x12u8; 32],
            "nonce": vec![0x34u8; 24],
            "ciphertext": vec![0x56u8; 32],
            "attribution_sk": vec![0x78u8; 32],
            "signature": vec![0x9au8; 64],
        });
        let legacy: Seal = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.sender_pubkey, seal.sender_pubkey);
        assert_eq!(legacy.nonce, seal.nonce);
        assert_eq!(legacy.ciphertext, seal.ciphertext);
        assert_eq!(legacy.attribution_sk, seal.attribution_sk);
        assert_eq!(legacy.signature, seal.signature);

        let rumor = Rumor {
            from: [0xab; 32],
            to: [0xcd; 32],
            created_at: NOW,
            body: "fixed-width identity fields".to_owned(),
        };
        let encoded_rumor = serde_json::to_value(&rumor).unwrap();
        assert_eq!(encoded_rumor["from"].as_str().unwrap(), "ab".repeat(32));
        assert_eq!(encoded_rumor["to"].as_str().unwrap(), "cd".repeat(32));
        let legacy_rumor: Rumor = serde_json::from_value(serde_json::json!({
            "from": vec![0xabu8; 32],
            "to": vec![0xcdu8; 32],
            "created_at": NOW,
            "body": "fixed-width identity fields",
        }))
        .unwrap();
        assert_eq!(legacy_rumor, rumor);

        for malformed in [
            serde_json::json!("AA".repeat(24)),
            serde_json::json!("00".repeat(23)),
            serde_json::json!("00".repeat(25)),
            serde_json::json!(vec![0u8; 23]),
            serde_json::json!(vec![0u8; 25]),
        ] {
            assert!(serde_json::from_value::<FixedBytes<24>>(malformed).is_err());
        }
    }

    #[test]
    fn frozen_pre_codec_v3_array_fields_remain_readable() {
        // Produced by the immediately preceding v3 writer, whose fixed-size rumor and seal
        // fields were decimal arrays. Keeping the complete encrypted wrap frozen proves that
        // already-stored v3 messages survive the writer's fixed-width encoding change.
        let wrapped: Wrap = serde_json::from_str(include_str!(
            "../tests/fixtures/legacy-v3-array-fields.json"
        ))
        .unwrap();
        let (alice, bob) = pair();

        let (sender, body) = open(&bob, &wrapped).unwrap();

        assert_eq!(sender, alice.verifying_key());
        assert_eq!(body.as_str(), "frozen pre-codec v3");
        assert_eq!(wrapped.created_at, NOW - 123);
    }

    #[test]
    fn padding_round_trips_at_block_boundaries() {
        for len in [0usize, 1, 251, 252, 253, 511, 512, 513] {
            let data = vec![7u8; len];
            let padded = pad(&data);
            assert_eq!(padded.len() % PAD_BLOCK, 0);
            assert_eq!(unpad(&padded), data, "len {len}");
        }
    }

    #[test]
    fn oversized_bodies_are_refused() {
        let (alice, bob) = pair();
        let huge = "x".repeat(MAX_PLAINTEXT + 1);
        assert_eq!(
            wrap(&alice, &bob.verifying_key(), &huge, NOW),
            Err(Error::TooLarge)
        );
    }

    #[test]
    fn debug_output_withholds_plaintext_routing_and_attribution_material() {
        let (alice, bob) = pair();
        let rumor = Rumor {
            from: [0xA1; 32],
            to: [0xB2; 32],
            created_at: NOW,
            body: "rumor-debug-canary-7193".to_owned(),
        };
        assert_eq!(format!("{rumor:?}"), "Rumor(<withheld>)");

        let wrapped = wrap(&alice, &bob.verifying_key(), "wrap-debug-canary-7193", NOW).unwrap();
        let wrap_debug = format!("{wrapped:?}");
        assert_eq!(wrap_debug, "Wrap { version: 3, contents: \"withheld\" }");
        assert!(!wrap_debug.contains("wrap-debug-canary-7193"));

        let (_, compliance_public_key) = compliance(0xC3);
        let attributed = attributed(&alice, &bob, &compliance_public_key);
        assert_eq!(
            format!("{:?}", attributed.attribution.as_ref().unwrap()),
            "AttributionBlock::V3(<withheld>)"
        );
        let legacy = AttributionBlockV2 {
            epoch_id: 7,
            e_pk: [0xD4; 32],
            nonce: [0xE5; 24],
            ciphertext: b"attribution-debug-canary-7193".to_vec(),
        };
        assert_eq!(format!("{legacy:?}"), "AttributionBlockV2(<withheld>)");
    }

    #[test]
    fn unpad_survives_hostile_length_prefixes() {
        assert!(unpad(&[]).is_empty());
        assert!(unpad(&[0xff, 0xff, 0xff, 0xff]).is_empty());
        assert!(unpad(&[0xff, 0xff, 0xff, 0xff, 1, 2, 3]).is_empty());
    }

    // ---- versioning and attribution ---------------------------------------------------------

    use pigeonpost_compliance_format::{CompliancePurpose, Jurisdiction};

    /// A compliance keypair. `P_c` is public; custody holds the returned identity's secret.
    fn compliance(seed: u8) -> (Identity, [u8; 32]) {
        let holder = Identity::from_seed([seed; 32]);
        let p_c = crate::keys::x25519_public(&holder);
        (holder, p_c)
    }

    fn key_id(generation: u32) -> ComplianceKeyId {
        ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [0xA5; 32],
            1_785_542_400_000,
            generation,
        )
    }

    fn attributed(sender: &Identity, recipient: &Identity, p_c: &[u8; 32]) -> Wrap {
        wrap_attributed(
            sender,
            &recipient.verifying_key(),
            "hello",
            NOW,
            p_c,
            &key_id(7),
        )
        .unwrap()
    }

    fn open_seal(recipient: &Identity, wrapped: &Wrap) -> Seal {
        let ephemeral_pubkey = VerifyingKey::from_bytes(&wrapped.ephemeral_pubkey).unwrap();
        let wrap_key = derive_key(&recipient.agree(&ephemeral_pubkey).unwrap(), HKDF_INFO_WRAP);
        let seal_bytes = unpad(
            &decrypt(
                &wrap_key,
                &wrapped.nonce,
                &wrapped.ciphertext,
                wrapped.version,
            )
            .unwrap(),
        );
        serde_json::from_slice(&seal_bytes).unwrap()
    }

    /// Deterministic generator for a genuine legacy wrap. Keeping generation test-only prevents
    /// production callers from writing v2 while still pinning every legacy read primitive.
    fn legacy_v2_wrap(sender: &Identity, recipient: &Identity, attributed: bool) -> Wrap {
        #[derive(Serialize)]
        struct LegacyRumor<'a> {
            from: [u8; 32],
            to: [u8; 32],
            created_at: u64,
            body: &'a str,
        }

        #[derive(Serialize)]
        struct LegacySeal {
            sender_pubkey: [u8; 32],
            nonce: [u8; 24],
            ciphertext: Vec<u8>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            attribution_sk: Option<[u8; 32]>,
            #[serde(with = "serde_big_array::BigArray")]
            signature: [u8; 64],
        }

        let rumor = LegacyRumor {
            from: sender.verifying_key().to_bytes(),
            to: recipient.verifying_key().to_bytes(),
            created_at: NOW,
            body: "legacy v2",
        };
        let rumor_bytes = serde_json::to_vec(&rumor).unwrap();
        let seal_key = derive_key(
            &sender.agree(&recipient.verifying_key()).unwrap(),
            HKDF_INFO_SEAL,
        );
        let seal_nonce = [0x31; 24];
        let seal_ct = encrypt(
            &seal_key,
            &seal_nonce,
            &pad(&rumor_bytes),
            LEGACY_ENVELOPE_VERSION,
        )
        .unwrap();
        let attribution_sk = attributed.then_some([0x42; 32]);
        let seal = LegacySeal {
            sender_pubkey: sender.verifying_key().to_bytes(),
            nonce: seal_nonce,
            ciphertext: seal_ct.clone(),
            attribution_sk,
            signature: sender
                .sign(&seal_signing_payload(
                    &seal_nonce,
                    &seal_ct,
                    attribution_sk.as_ref(),
                ))
                .to_bytes(),
        };
        let seal_bytes = serde_json::to_vec(&seal).unwrap();

        let ephemeral = Identity::from_seed([0x44; 32]);
        let wrap_key = derive_key(
            &ephemeral.agree(&recipient.verifying_key()).unwrap(),
            HKDF_INFO_WRAP,
        );
        let nonce = [0x32; 24];
        let ciphertext = encrypt(
            &wrap_key,
            &nonce,
            &pad(&seal_bytes),
            LEGACY_ENVELOPE_VERSION,
        )
        .unwrap();
        let attribution = attributed.then(|| {
            let e_pk = keys::x25519_public(&Identity::from_seed([0x42; 32]));
            AttributionBlock::V2(AttributionBlockV2 {
                epoch_id: 7,
                e_pk,
                nonce: [0x33; 24],
                ciphertext: vec![0x34; 64],
            })
        });
        let created_at = NOW - 17;
        let recipient = recipient.verifying_key().to_bytes();
        let signature = ephemeral
            .sign(&wrap_signing_payload_v2(
                LEGACY_ENVELOPE_VERSION,
                &recipient,
                created_at,
                &nonce,
                &ciphertext,
                attribution.as_ref(),
            ))
            .to_bytes();
        Wrap {
            version: LEGACY_ENVELOPE_VERSION,
            ephemeral_pubkey: ephemeral.verifying_key().to_bytes(),
            recipient,
            nonce,
            ciphertext,
            created_at,
            signature,
            pow_nonce: 0,
            attribution,
        }
    }

    fn forge_victim_claim(
        signer: &Identity,
        claimed_sender: &VerifyingKey,
        p_c: &[u8; 32],
        e_sk: &[u8; 32],
        event_id: &[u8; 32],
        recipient: &[u8; 32],
        created_at: u64,
    ) -> AttributionBlock {
        let ephemeral = Identity::from_seed(*e_sk);
        let e_pk = keys::x25519_public(&ephemeral);
        let digest: [u8; 32] = Sha256::digest(p_c).into();
        let id = key_id(7);
        let sent_at_ms = created_at * 1_000;
        let preimage = attribution_signing_preimage(
            ATTRIBUTION_BLOCK_VERSION,
            &id,
            &digest,
            &e_pk,
            event_id,
            recipient,
            sent_at_ms,
        )
        .unwrap();
        let claim = AttributionClaim {
            sender_pubkey: claimed_sender.to_bytes(),
            sent_at_ms,
            signature: signer.sign(&preimage).to_bytes(),
        };
        let shared = keys::x25519_agree(&ephemeral, p_c).unwrap();
        let key = derive_attr_key_v3(&shared, &e_pk, &digest);
        let nonce = [0x55; 24];
        let ciphertext: [u8; ATTRIBUTION_CIPHERTEXT_LEN] = XChaCha20Poly1305::new(&key.into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &claim.encode(),
                    aad: &attribution_aad(
                        ATTRIBUTION_BLOCK_VERSION,
                        &id,
                        &digest,
                        &e_pk,
                        event_id,
                        recipient,
                    )
                    .unwrap(),
                },
            )
            .unwrap()
            .try_into()
            .unwrap();
        AttributionBlock::V3(AttributionBlockV3 {
            block_version: ATTRIBUTION_BLOCK_VERSION,
            key_id: id,
            compliance_key_digest: digest,
            e_pk,
            nonce,
            ciphertext,
        })
    }

    #[test]
    fn writers_emit_v3_and_attribution_round_trips() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = attributed(&alice, &bob, &p_c);

        assert_eq!(wrapped.version, ENVELOPE_VERSION);
        let block = wrapped.attribution.as_ref().unwrap().as_v3().unwrap();
        assert_eq!(block.block_version, ATTRIBUTION_BLOCK_VERSION);
        assert_eq!(block.key_id, key_id(7));
        assert_eq!(block.ciphertext.len(), ATTRIBUTION_CIPHERTEXT_LEN);

        let (sender, body, attribution) = open_attributed(&bob, &wrapped, Some(&p_c)).unwrap();
        assert_eq!(sender, alice.verifying_key());
        assert_eq!(body.as_str(), "hello");
        assert_eq!(attribution, Attribution::Valid);
    }

    #[test]
    fn public_validator_accepts_v3_without_decrypting() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let attributed = attributed(&alice, &bob, &p_c);
        let plain = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();

        assert!(attributed.verify_public().is_ok());
        assert!(plain.verify_public().is_ok());
    }

    #[test]
    fn wrap_json_rejects_unknown_nested_fields() {
        let (alice, bob) = pair();
        let wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
        let mut value = serde_json::to_value(&wrapped).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_ambiguous_field".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<Wrap>(value).is_err());
    }

    #[test]
    fn v3_json_uses_compact_ciphertext_and_v2_keeps_the_deployed_array_shape() {
        let (alice, bob) = pair();
        let current = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
        let current_json = serde_json::to_value(&current).unwrap();
        let encoded = current_json["ciphertext"].as_str().unwrap();
        assert_eq!(encoded.len(), current.ciphertext.len() * 2);
        assert!(encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert_eq!(
            serde_json::from_value::<Wrap>(current_json).unwrap(),
            current
        );

        let legacy = legacy_v2_wrap(&alice, &bob, false);
        let legacy_json = serde_json::to_value(&legacy).unwrap();
        assert!(legacy_json["ciphertext"].is_array());
        assert_eq!(serde_json::from_value::<Wrap>(legacy_json).unwrap(), legacy);
    }

    #[test]
    fn public_validator_rejects_legacy_malformed_and_tampered_writes() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let legacy = legacy_v2_wrap(&alice, &bob, false);
        assert_eq!(
            legacy.verify_public(),
            Err(Error::MalformedEnvelope("v3 required for publish"))
        );

        let wrapped = attributed(&alice, &bob, &p_c);
        let mut changed = wrapped.clone();
        changed.created_at ^= 1;
        assert_eq!(changed.verify_public(), Err(Error::BadSignature));

        let mut changed = wrapped.clone();
        changed.ciphertext.truncate(17);
        assert_eq!(
            changed.verify_public(),
            Err(Error::MalformedEnvelope("outer ciphertext length"))
        );

        let mut changed = wrapped.clone();
        let AttributionBlock::V3(block) = changed.attribution.as_mut().unwrap() else {
            unreachable!()
        };
        block.e_pk = [0; 32];
        assert_eq!(changed.verify_public(), Err(Error::InvalidKey));

        let mut changed = wrapped.clone();
        let AttributionBlock::V3(block) = changed.attribution.as_mut().unwrap() else {
            unreachable!()
        };
        block.compliance_key_digest = [0; 32];
        assert_eq!(
            changed.verify_public(),
            Err(Error::MalformedEnvelope("empty compliance key digest"))
        );
    }

    #[test]
    fn public_aad_context_needs_no_recipient_secret_or_inner_seal() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = attributed(&alice, &bob, &p_c);
        let block = wrapped.attribution.as_ref().unwrap().as_v3().unwrap();

        let public_context = attribution_aad(
            block.block_version,
            &block.key_id,
            &block.compliance_key_digest,
            &block.e_pk,
            &wrapped.id(),
            &wrapped.recipient,
        )
        .unwrap();
        assert!(!public_context.is_empty());
        assert!(public_context
            .windows(32)
            .any(|bytes| bytes == wrapped.id()));
        assert!(public_context
            .windows(32)
            .any(|bytes| bytes == wrapped.recipient));
    }

    #[test]
    fn victim_key_forged_claim_is_rejected() {
        let (alice, bob) = pair();
        let mallory = Identity::from_seed([0x99; 32]);
        let (_, p_c) = compliance(0xC1);
        let event_id = [0x61; 32];
        let e_sk = [0x62; 32];
        let block = forge_victim_claim(
            &mallory,
            &alice.verifying_key(),
            &p_c,
            &e_sk,
            &event_id,
            &bob.verifying_key().to_bytes(),
            NOW,
        );

        assert_eq!(
            verify_attribution(
                &block,
                &e_sk,
                &p_c,
                &event_id,
                &bob.verifying_key().to_bytes(),
                NOW,
                &alice.verifying_key(),
            ),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn every_public_attribution_binding_is_enforced() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = attributed(&alice, &bob, &p_c);
        let seal = open_seal(&bob, &wrapped);
        let e_sk = seal.attribution_sk.as_ref().unwrap();
        let block = wrapped.attribution.as_ref().unwrap();
        let event_id = wrapped.id();

        assert!(verify_attribution(
            block,
            e_sk,
            &p_c,
            &event_id,
            &wrapped.recipient,
            wrapped.created_at,
            &alice.verifying_key(),
        )
        .is_ok());

        let mut wrong_event = event_id;
        wrong_event[0] ^= 1;
        assert_eq!(
            verify_attribution(
                block,
                e_sk,
                &p_c,
                &wrong_event,
                &wrapped.recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::DecryptionFailed)
        );

        let mut wrong_recipient = wrapped.recipient;
        wrong_recipient[0] ^= 1;
        assert_eq!(
            verify_attribution(
                block,
                e_sk,
                &p_c,
                &event_id,
                &wrong_recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::DecryptionFailed)
        );

        let mut changed = block.clone();
        let AttributionBlock::V3(changed) = &mut changed else {
            unreachable!()
        };
        changed.key_id.generation += 1;
        assert_eq!(
            verify_attribution(
                &AttributionBlock::V3(changed.clone()),
                e_sk,
                &p_c,
                &event_id,
                &wrapped.recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::DecryptionFailed)
        );

        let mut changed = block.clone();
        let AttributionBlock::V3(changed) = &mut changed else {
            unreachable!()
        };
        changed.compliance_key_digest[0] ^= 1;
        assert_eq!(
            verify_attribution(
                &AttributionBlock::V3(changed.clone()),
                e_sk,
                &p_c,
                &event_id,
                &wrapped.recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::MalformedEnvelope("compliance key digest mismatch"))
        );

        let mut changed = block.clone();
        let AttributionBlock::V3(changed) = &mut changed else {
            unreachable!()
        };
        changed.block_version = 4;
        assert_eq!(
            verify_attribution(
                &AttributionBlock::V3(changed.clone()),
                e_sk,
                &p_c,
                &event_id,
                &wrapped.recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::MalformedEnvelope("attribution version or key id"))
        );

        let mut changed = block.clone();
        let AttributionBlock::V3(changed) = &mut changed else {
            unreachable!()
        };
        changed.e_pk[0] ^= 1;
        assert_eq!(
            verify_attribution(
                &AttributionBlock::V3(changed.clone()),
                e_sk,
                &p_c,
                &event_id,
                &wrapped.recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::MalformedEnvelope("attribution key mismatch"))
        );

        let mut changed = block.clone();
        let AttributionBlock::V3(changed) = &mut changed else {
            unreachable!()
        };
        changed.ciphertext[0] ^= 1;
        assert_eq!(
            verify_attribution(
                &AttributionBlock::V3(changed.clone()),
                e_sk,
                &p_c,
                &event_id,
                &wrapped.recipient,
                wrapped.created_at,
                &alice.verifying_key(),
            ),
            Err(Error::DecryptionFailed)
        );
    }

    #[test]
    fn attribution_time_is_bounded_to_visible_time_plus_jitter_and_skew() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let e_sk = [0x71; 32];
        let event_id = [0x72; 32];
        let recipient = bob.verifying_key().to_bytes();

        let early = AttributionBlock::V3(
            build_attribution_v3(
                &alice,
                NOW - 1,
                &p_c,
                &key_id(7),
                &e_sk,
                &event_id,
                &recipient,
            )
            .unwrap(),
        );
        assert_eq!(
            verify_attribution(
                &early,
                &e_sk,
                &p_c,
                &event_id,
                &recipient,
                NOW,
                &alice.verifying_key(),
            ),
            Err(Error::StaleTimestamp)
        );

        let too_late = NOW + MAX_TIMESTAMP_JITTER_SECS + MAX_ATTRIBUTION_CLOCK_SKEW_MS / 1_000 + 1;
        let late = AttributionBlock::V3(
            build_attribution_v3(
                &alice,
                too_late,
                &p_c,
                &key_id(7),
                &e_sk,
                &event_id,
                &recipient,
            )
            .unwrap(),
        );
        assert_eq!(
            verify_attribution(
                &late,
                &e_sk,
                &p_c,
                &event_id,
                &recipient,
                NOW,
                &alice.verifying_key(),
            ),
            Err(Error::StaleTimestamp)
        );
    }

    #[test]
    fn attribution_true_send_time_not_jittered_visible_time_selects_monthly_epoch() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let e_sk = [0x73; 32];
        let event_id = [0x74; 32];
        let recipient = bob.verifying_key().to_bytes();
        const AUGUST_2026_START_SECS: u64 = 1_785_542_400;
        let sent_at = AUGUST_2026_START_SECS + 60 * 60;
        let visible_created_at = sent_at - 2 * 60 * 60;
        let key_id = key_id(7);
        assert_eq!(
            attribution_epoch_contains(&key_id, sent_at * 1_000),
            Ok(true)
        );
        assert_eq!(
            attribution_epoch_contains(&key_id, visible_created_at * 1_000),
            Ok(false),
            "privacy jitter intentionally crosses into the preceding month"
        );

        let block = AttributionBlock::V3(
            build_attribution_v3(&alice, sent_at, &p_c, &key_id, &e_sk, &event_id, &recipient)
                .unwrap(),
        );
        assert_eq!(
            verify_attribution(
                &block,
                &e_sk,
                &p_c,
                &event_id,
                &recipient,
                visible_created_at,
                &alice.verifying_key(),
            ),
            Ok(())
        );
    }

    #[test]
    fn wrong_or_unresolved_compliance_key_is_invalid() {
        let (alice, bob) = pair();
        let (_, expected_p_c) = compliance(0xC1);
        let (_, wrong_p_c) = compliance(0xC2);
        let wrapped = attributed(&alice, &bob, &wrong_p_c);

        assert_eq!(
            open_attributed(&bob, &wrapped, Some(&expected_p_c))
                .unwrap()
                .2,
            Attribution::Invalid
        );
        assert_eq!(
            open_attributed(&bob, &wrapped, None).unwrap().2,
            Attribution::Invalid
        );
    }

    #[test]
    fn trusted_attribution_requires_exact_registry_key_and_published_interval() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = attributed(&alice, &bob, &p_c);
        const AUGUST_2026: u64 = 1_785_542_400_000;
        const SEPTEMBER_2026: u64 = 1_788_220_800_000;

        let trusted =
            TrustedAttributionKey::new(key_id(7), p_c, AUGUST_2026, SEPTEMBER_2026).unwrap();
        assert_eq!(
            open_attributed_trusted(&bob, &wrapped, Some(&trusted))
                .unwrap()
                .2,
            Attribution::Valid
        );

        let wrong_id =
            TrustedAttributionKey::new(key_id(8), p_c, AUGUST_2026, SEPTEMBER_2026).unwrap();
        assert_eq!(
            open_attributed_trusted(&bob, &wrapped, Some(&wrong_id))
                .unwrap()
                .2,
            Attribution::Invalid
        );

        assert!(
            TrustedAttributionKey::new(key_id(7), p_c, AUGUST_2026 + 1, SEPTEMBER_2026,).is_err()
        );
    }

    #[test]
    fn unattributed_is_absent_not_invalid() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
        assert_eq!(
            open_attributed(&bob, &wrapped, Some(&p_c)).unwrap().2,
            Attribution::Absent
        );
    }

    #[test]
    fn stripping_v3_block_breaks_signature_but_not_event_id() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let mut wrapped = attributed(&alice, &bob, &p_c);
        let event_id = wrapped.id();
        wrapped.attribution = None;

        assert_eq!(wrapped.id(), event_id);
        assert_eq!(
            open_attributed(&bob, &wrapped, Some(&p_c)),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn v3_block_serialization_has_fixed_ciphertext_and_rejects_unknown_version() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = attributed(&alice, &bob, &p_c);
        let serialized = serde_json::to_vec(&wrapped).unwrap();
        let decoded: Wrap = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(decoded, wrapped);
        assert_eq!(
            decoded.attribution.as_ref().unwrap().ciphertext_len(),
            ATTRIBUTION_CIPHERTEXT_LEN
        );

        let mut value = serde_json::to_value(&wrapped).unwrap();
        value["attribution"]["block_version"] = serde_json::json!(4);
        assert!(serde_json::from_value::<Wrap>(value).is_err());
    }

    #[test]
    fn v2_read_signature_id_and_open_are_compatible() {
        let (alice, bob) = pair();
        let wrapped = legacy_v2_wrap(&alice, &bob, false);
        assert_eq!(
            wrapped.id(),
            [
                0x5b, 0x86, 0x02, 0x0a, 0xcf, 0xf6, 0x4b, 0xe7, 0x23, 0xb4, 0x4d, 0xf5, 0xab, 0x02,
                0xab, 0x86, 0xdf, 0x30, 0xdb, 0x55, 0x5b, 0xd1, 0xfb, 0x5f, 0x14, 0x63, 0x1f, 0xf6,
                0x29, 0x2d, 0xda, 0x09,
            ]
        );
        let encoded = serde_json::to_vec(&wrapped).unwrap();
        let decoded: Wrap = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.id(), wrapped.id());
        let (sender, body) = open(&bob, &decoded).unwrap();
        assert_eq!(sender, alice.verifying_key());
        assert_eq!(body.as_str(), "legacy v2");
    }

    #[test]
    fn v2_attribution_is_readable_but_never_compliance_valid() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = legacy_v2_wrap(&alice, &bob, true);
        let (sender, body, attribution) = open_attributed(&bob, &wrapped, Some(&p_c)).unwrap();
        assert_eq!(sender, alice.verifying_key());
        assert_eq!(body.as_str(), "legacy v2");
        assert_eq!(attribution, Attribution::Invalid);
    }

    #[test]
    fn v1_is_rejected() {
        let (alice, bob) = pair();
        let mut wrapped = legacy_v2_wrap(&alice, &bob, false);
        wrapped.version = 1;
        assert_eq!(
            open(&bob, &wrapped),
            Err(Error::MalformedEnvelope("unsupported version"))
        );
    }

    #[test]
    fn v3_outer_fields_and_complete_block_are_signed() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let wrapped = attributed(&alice, &bob, &p_c);

        let mut mutations = Vec::new();
        let mut changed = wrapped.clone();
        changed.ephemeral_pubkey[0] ^= 1;
        mutations.push(changed);
        let mut changed = wrapped.clone();
        changed.nonce[0] ^= 1;
        mutations.push(changed);
        let mut changed = wrapped.clone();
        changed.ciphertext[0] ^= 1;
        mutations.push(changed);
        let mut changed = wrapped.clone();
        changed.created_at -= 1;
        mutations.push(changed);
        let mut changed = wrapped.clone();
        let AttributionBlock::V3(block) = changed.attribution.as_mut().unwrap() else {
            unreachable!()
        };
        block.key_id.generation += 1;
        mutations.push(changed);
        let mut changed = wrapped.clone();
        let AttributionBlock::V3(block) = changed.attribution.as_mut().unwrap() else {
            unreachable!()
        };
        block.ciphertext[0] ^= 1;
        mutations.push(changed);

        assert!(mutations
            .iter()
            .all(|changed| open_attributed(&bob, changed, Some(&p_c)).is_err()));
    }

    #[test]
    fn proof_of_work_and_privacy_hold_for_v3_attribution() {
        let (alice, bob) = pair();
        let (_, p_c) = compliance(0xC1);
        let mut wrapped = attributed(&alice, &bob, &p_c);
        let stored = serde_json::to_vec(&wrapped).unwrap();
        assert!(!stored
            .windows(32)
            .any(|bytes| bytes == alice.verifying_key().as_bytes()));
        assert!(!String::from_utf8_lossy(&stored).contains("hello"));

        wrapped.stamp(8, 5_000_000).unwrap();
        assert!(wrapped.verify_work(8).is_ok());
    }

    #[test]
    fn recipient_field_cannot_be_altered() {
        let (alice, bob) = pair();
        let carol = Identity::from_seed([0x5C; 32]);
        let mut wrapped = wrap(&alice, &bob.verifying_key(), "hello", NOW).unwrap();
        wrapped.recipient = carol.verifying_key().to_bytes();
        assert!(open(&bob, &wrapped).is_err());
        assert!(open(&carol, &wrapped).is_err());
    }
}
