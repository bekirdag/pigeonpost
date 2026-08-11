//! Canonical compliance record formats shared by online and offline components.
//!
//! This crate deliberately contains no cryptography. It specifies byte-level formats and public
//! signing/AAD preimages; callers choose and apply the cryptographic primitives. Keeping secret-key
//! operations out of this dependency makes the online/offline boundary enforceable by Cargo.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use core::fmt;

use serde::{Deserialize, Serialize};

/// Version of the canonical [`ComplianceKeyId`] encoding.
pub const COMPLIANCE_KEY_ID_VERSION: u8 = 1;
/// Exact byte length of a canonical [`ComplianceKeyId`].
pub const COMPLIANCE_KEY_ID_LEN: usize = 47;
/// Exact duration of every network- or identity-trace epoch: one UTC day.
pub const TRACE_EPOCH_DURATION_MS: u64 = 86_400_000;
/// Version of the first independently verifiable attribution block.
pub const ATTRIBUTION_BLOCK_VERSION: u8 = 3;
/// Exact byte length of an [`AttributionClaim`].
pub const ATTRIBUTION_CLAIM_LEN: usize = 104;
/// XChaCha20-Poly1305 appends a 16-byte tag to the fixed claim.
pub const ATTRIBUTION_CIPHERTEXT_LEN: usize = ATTRIBUTION_CLAIM_LEN + 16;
/// HKDF `info` for the v3 attribution AEAD key.
pub const ATTRIBUTION_HKDF_INFO: &[u8] = b"pigeonpost/envelope/v3/attribution";

const ATTRIBUTION_SIGNATURE_DOMAIN: &[u8] = b"pigeonpost/attribution-claim/v3";
const ATTRIBUTION_AAD_DOMAIN: &[u8] = b"pigeonpost/attribution-aad/v3";

/// Why a compliance key exists. Purpose separation prevents one custody key from becoming a
/// universal decryption capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum CompliancePurpose {
    Attribution = 1,
    NetworkTrace = 2,
    IdentityTrace = 3,
}

impl TryFrom<u8> for CompliancePurpose {
    type Error = FormatError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Attribution),
            2 => Ok(Self::NetworkTrace),
            3 => Ok(Self::IdentityTrace),
            _ => Err(FormatError::UnknownPurpose(value)),
        }
    }
}

impl From<CompliancePurpose> for u8 {
    fn from(value: CompliancePurpose) -> Self {
        value as u8
    }
}

/// Jurisdiction whose custody and retention policy governs a compliance key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Jurisdiction {
    Us = 1,
    Eu = 2,
    Tr = 3,
    Test = 255,
}

impl TryFrom<u8> for Jurisdiction {
    type Error = FormatError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Us),
            2 => Ok(Self::Eu),
            3 => Ok(Self::Tr),
            255 => Ok(Self::Test),
            _ => Err(FormatError::UnknownJurisdiction(value)),
        }
    }
}

impl From<Jurisdiction> for u8 {
    fn from(value: Jurisdiction) -> Self {
        value as u8
    }
}

/// Regulatory capture mode shared by every online trace producer and serving-boundary validator.
///
/// Keeping this policy in the cryptography-free format crate prevents the loft, registry, and
/// operator CLI from drifting onto subtly different UTC-runway calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceCapturePolicy {
    Standing,
    Preservation {
        starts_at_ms: u64,
        expires_at_ms: u64,
    },
}

impl TraceCapturePolicy {
    pub fn captures(self, timestamp_ms: u64) -> bool {
        match self {
            Self::Standing => true,
            Self::Preservation {
                starts_at_ms,
                expires_at_ms,
            } => starts_at_ms <= timestamp_ms && timestamp_ms < expires_at_ms,
        }
    }
}

/// Complete jurisdictional policy carried by an immutable online trace-capacity contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRetentionPolicy {
    pub jurisdiction: Jurisdiction,
    pub capture: TraceCapturePolicy,
    /// US standing capture is exactly 30 days, Türkiye standing capture is the counsel-selected
    /// 365–730 days, and preservation/test policies carry no standing-retention value.
    pub retention_days: Option<u64>,
}

impl TraceRetentionPolicy {
    /// Return the minimum canonical UTC-key runway required by this exact policy.
    pub fn required_capacity_epochs(self) -> Result<u64, FormatError> {
        match (self.jurisdiction, self.capture, self.retention_days) {
            (
                Jurisdiction::Eu,
                TraceCapturePolicy::Preservation {
                    starts_at_ms,
                    expires_at_ms,
                },
                None,
            ) if starts_at_ms > 0 && starts_at_ms < expires_at_ms => {
                let first_epoch = starts_at_ms / TRACE_EPOCH_DURATION_MS;
                let last_epoch = expires_at_ms
                    .checked_sub(1)
                    .ok_or(FormatError::InvalidTraceRetentionPolicy)?
                    / TRACE_EPOCH_DURATION_MS;
                last_epoch
                    .checked_sub(first_epoch)
                    .and_then(|epochs| epochs.checked_add(1))
                    .ok_or(FormatError::InvalidTraceRetentionPolicy)
            }
            (Jurisdiction::Us, TraceCapturePolicy::Standing, Some(30)) => Ok(31),
            (Jurisdiction::Tr, TraceCapturePolicy::Standing, Some(days @ 365..=730)) => days
                .checked_add(1)
                .ok_or(FormatError::InvalidTraceRetentionPolicy),
            (Jurisdiction::Test, TraceCapturePolicy::Standing, None) => Ok(1),
            _ => Err(FormatError::InvalidTraceRetentionPolicy),
        }
    }
}

/// Globally unambiguous identifier for a purpose- and jurisdiction-scoped compliance key.
///
/// The canonical integer encoding is big-endian so lexicographic order follows epoch and
/// generation order. `authority` is the stable 32-byte identifier of the publishing authority,
/// not the compliance public key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplianceKeyId {
    pub version: u8,
    pub purpose: CompliancePurpose,
    pub jurisdiction: Jurisdiction,
    pub authority: [u8; 32],
    pub epoch_start_ms: u64,
    pub generation: u32,
}

impl ComplianceKeyId {
    pub const fn new(
        purpose: CompliancePurpose,
        jurisdiction: Jurisdiction,
        authority: [u8; 32],
        epoch_start_ms: u64,
        generation: u32,
    ) -> Self {
        Self {
            version: COMPLIANCE_KEY_ID_VERSION,
            purpose,
            jurisdiction,
            authority,
            epoch_start_ms,
            generation,
        }
    }

    /// Validate and encode to the one canonical byte representation.
    pub fn encode(&self) -> Result<[u8; COMPLIANCE_KEY_ID_LEN], FormatError> {
        self.validate()?;
        let mut out = [0u8; COMPLIANCE_KEY_ID_LEN];
        out[0] = self.version;
        out[1] = self.purpose.into();
        out[2] = self.jurisdiction.into();
        out[3..35].copy_from_slice(&self.authority);
        out[35..43].copy_from_slice(&self.epoch_start_ms.to_be_bytes());
        out[43..47].copy_from_slice(&self.generation.to_be_bytes());
        Ok(out)
    }

    /// Decode an exact canonical encoding, rejecting trailing bytes and unknown discriminants.
    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() != COMPLIANCE_KEY_ID_LEN {
            return Err(FormatError::InvalidLength {
                expected: COMPLIANCE_KEY_ID_LEN,
                actual: bytes.len(),
            });
        }
        if bytes[0] != COMPLIANCE_KEY_ID_VERSION {
            return Err(FormatError::UnknownKeyIdVersion(bytes[0]));
        }
        let mut authority = [0u8; 32];
        authority.copy_from_slice(&bytes[3..35]);
        let epoch_start_ms = u64::from_be_bytes(
            bytes[35..43]
                .try_into()
                .expect("the fixed slice has exactly eight bytes"),
        );
        let generation = u32::from_be_bytes(
            bytes[43..47]
                .try_into()
                .expect("the fixed slice has exactly four bytes"),
        );
        Ok(Self {
            version: bytes[0],
            purpose: CompliancePurpose::try_from(bytes[1])?,
            jurisdiction: Jurisdiction::try_from(bytes[2])?,
            authority,
            epoch_start_ms,
            generation,
        })
    }

    pub fn validate(&self) -> Result<(), FormatError> {
        if self.version != COMPLIANCE_KEY_ID_VERSION {
            return Err(FormatError::UnknownKeyIdVersion(self.version));
        }
        Ok(())
    }
}

/// Return the exclusive end of a canonical daily trace epoch.
///
/// This is the single validator used by registry publication, online sealing, and offline
/// custody. A trace key starts on a UTC-day boundary and lasts exactly 86,400,000 milliseconds.
pub fn trace_epoch_end_ms(key_id: &ComplianceKeyId) -> Result<u64, FormatError> {
    if !matches!(
        key_id.purpose,
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace
    ) {
        return Err(FormatError::NotTracePurpose);
    }
    key_id.validate()?;
    if key_id.epoch_start_ms % TRACE_EPOCH_DURATION_MS != 0 {
        return Err(FormatError::InvalidTraceEpoch);
    }
    key_id
        .epoch_start_ms
        .checked_add(TRACE_EPOCH_DURATION_MS)
        .ok_or(FormatError::InvalidTraceEpoch)
}

/// Validate the exact published validity interval for a daily trace key.
pub fn validate_trace_epoch(
    key_id: &ComplianceKeyId,
    not_before_ms: u64,
    not_after_ms: u64,
) -> Result<(), FormatError> {
    let expected_end = trace_epoch_end_ms(key_id)?;
    if not_before_ms != key_id.epoch_start_ms || not_after_ms != expected_end {
        return Err(FormatError::InvalidTraceEpoch);
    }
    Ok(())
}

/// Test whether a timestamp belongs to a canonical daily trace epoch.
pub fn trace_epoch_contains(
    key_id: &ComplianceKeyId,
    timestamp_ms: u64,
) -> Result<bool, FormatError> {
    let end = trace_epoch_end_ms(key_id)?;
    Ok(timestamp_ms >= key_id.epoch_start_ms && timestamp_ms < end)
}

/// Return the exclusive end of a canonical attribution epoch.
///
/// Attribution epochs start at 00:00:00 UTC on the first day of a Gregorian calendar month and
/// end at the corresponding boundary of the next month. Their duration is deliberately not a
/// fixed number of days.
pub fn attribution_epoch_end_ms(key_id: &ComplianceKeyId) -> Result<u64, FormatError> {
    if key_id.purpose != CompliancePurpose::Attribution {
        return Err(FormatError::NotAttributionPurpose);
    }
    key_id.validate()?;
    if key_id.epoch_start_ms % TRACE_EPOCH_DURATION_MS != 0 {
        return Err(FormatError::InvalidAttributionEpoch);
    }
    let days = i64::try_from(key_id.epoch_start_ms / TRACE_EPOCH_DURATION_MS)
        .map_err(|_| FormatError::InvalidAttributionEpoch)?;
    let (year, month, day) = civil_from_days(days);
    if day != 1 {
        return Err(FormatError::InvalidAttributionEpoch);
    }
    let (next_year, next_month) = if month == 12 {
        (
            year.checked_add(1)
                .ok_or(FormatError::InvalidAttributionEpoch)?,
            1,
        )
    } else {
        (year, month + 1)
    };
    let next_days =
        days_from_civil(next_year, next_month, 1).ok_or(FormatError::InvalidAttributionEpoch)?;
    u64::try_from(next_days)
        .ok()
        .and_then(|value| value.checked_mul(TRACE_EPOCH_DURATION_MS))
        .filter(|end| *end > key_id.epoch_start_ms)
        .ok_or(FormatError::InvalidAttributionEpoch)
}

/// Validate the exact published validity interval for a calendar-month attribution key.
pub fn validate_attribution_epoch(
    key_id: &ComplianceKeyId,
    not_before_ms: u64,
    not_after_ms: u64,
) -> Result<(), FormatError> {
    let expected_end = attribution_epoch_end_ms(key_id)?;
    if not_before_ms != key_id.epoch_start_ms || not_after_ms != expected_end {
        return Err(FormatError::InvalidAttributionEpoch);
    }
    Ok(())
}

/// Test whether a timestamp belongs to a canonical calendar-month attribution epoch.
pub fn attribution_epoch_contains(
    key_id: &ComplianceKeyId,
    timestamp_ms: u64,
) -> Result<bool, FormatError> {
    let end = attribution_epoch_end_ms(key_id)?;
    Ok(timestamp_ms >= key_id.epoch_start_ms && timestamp_ms < end)
}

/// Return the purpose-aware exclusive end of any canonical compliance epoch.
pub fn compliance_epoch_end_ms(key_id: &ComplianceKeyId) -> Result<u64, FormatError> {
    match key_id.purpose {
        CompliancePurpose::Attribution => attribution_epoch_end_ms(key_id),
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace => {
            trace_epoch_end_ms(key_id)
        }
    }
}

/// Validate a published compliance interval using its purpose-specific epoch contract.
pub fn validate_compliance_epoch(
    key_id: &ComplianceKeyId,
    not_before_ms: u64,
    not_after_ms: u64,
) -> Result<(), FormatError> {
    match key_id.purpose {
        CompliancePurpose::Attribution => {
            validate_attribution_epoch(key_id, not_before_ms, not_after_ms)
        }
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace => {
            validate_trace_epoch(key_id, not_before_ms, not_after_ms)
        }
    }
}

/// Test timestamp membership using the key's purpose-specific canonical epoch.
pub fn compliance_epoch_contains(
    key_id: &ComplianceKeyId,
    timestamp_ms: u64,
) -> Result<bool, FormatError> {
    match key_id.purpose {
        CompliancePurpose::Attribution => attribution_epoch_contains(key_id, timestamp_ms),
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace => {
            trace_epoch_contains(key_id, timestamp_ms)
        }
    }
}

// Howard Hinnant's proleptic-Gregorian civil-date algorithms, with Unix day zero as 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year.checked_sub(i64::from(month <= 2))?;
    let era = year.div_euclid(400);
    let year_of_era = year.checked_sub(era.checked_mul(400)?)?;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era
        .checked_mul(365)?
        .checked_add(year_of_era / 4)?
        .checked_sub(year_of_era / 100)?
        .checked_add(day_of_year)?;
    era.checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)
}

/// Fixed-shape plaintext encrypted inside an attribution block.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AttributionClaim {
    pub sender_pubkey: [u8; 32],
    pub sent_at_ms: u64,
    pub signature: [u8; 64],
}

impl core::fmt::Debug for AttributionClaim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // This value exists only after attribution custody has been opened. Sender, timestamp, and
        // signature are all disclosure plaintext.
        f.write_str("AttributionClaim(<withheld>)")
    }
}

impl AttributionClaim {
    /// Encode as `sender_pubkey || sent_at_ms_be || signature`.
    pub fn encode(&self) -> [u8; ATTRIBUTION_CLAIM_LEN] {
        let mut out = [0u8; ATTRIBUTION_CLAIM_LEN];
        out[..32].copy_from_slice(&self.sender_pubkey);
        out[32..40].copy_from_slice(&self.sent_at_ms.to_be_bytes());
        out[40..].copy_from_slice(&self.signature);
        out
    }

    /// Decode the exact fixed claim and reject truncated or extended records.
    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() != ATTRIBUTION_CLAIM_LEN {
            return Err(FormatError::InvalidLength {
                expected: ATTRIBUTION_CLAIM_LEN,
                actual: bytes.len(),
            });
        }
        let mut sender_pubkey = [0u8; 32];
        sender_pubkey.copy_from_slice(&bytes[..32]);
        let sent_at_ms = u64::from_be_bytes(
            bytes[32..40]
                .try_into()
                .expect("the fixed slice has exactly eight bytes"),
        );
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[40..]);
        Ok(Self {
            sender_pubkey,
            sent_at_ms,
            signature,
        })
    }
}

/// Construct the canonical sender-signature preimage for an attribution claim.
///
/// `compliance_key_digest` is SHA-256 of the resolved compliance public key. Hashing is done by
/// the caller so this format crate remains cryptography-free.
pub fn attribution_signing_preimage(
    block_version: u8,
    key_id: &ComplianceKeyId,
    compliance_key_digest: &[u8; 32],
    e_pk: &[u8; 32],
    event_id: &[u8; 32],
    recipient: &[u8; 32],
    sent_at_ms: u64,
) -> Result<Vec<u8>, FormatError> {
    validate_block_version(block_version)?;
    let key_id = key_id.encode()?;
    let mut out = Vec::with_capacity(
        ATTRIBUTION_SIGNATURE_DOMAIN.len() + 1 + COMPLIANCE_KEY_ID_LEN + 32 * 4 + 8,
    );
    out.extend_from_slice(ATTRIBUTION_SIGNATURE_DOMAIN);
    out.push(block_version);
    out.extend_from_slice(&key_id);
    out.extend_from_slice(compliance_key_digest);
    out.extend_from_slice(e_pk);
    out.extend_from_slice(event_id);
    out.extend_from_slice(recipient);
    out.extend_from_slice(&sent_at_ms.to_be_bytes());
    Ok(out)
}

/// Construct the canonical public AEAD context for an attribution block.
pub fn attribution_aad(
    block_version: u8,
    key_id: &ComplianceKeyId,
    compliance_key_digest: &[u8; 32],
    e_pk: &[u8; 32],
    event_id: &[u8; 32],
    recipient: &[u8; 32],
) -> Result<Vec<u8>, FormatError> {
    validate_block_version(block_version)?;
    let key_id = key_id.encode()?;
    let mut out =
        Vec::with_capacity(ATTRIBUTION_AAD_DOMAIN.len() + 1 + COMPLIANCE_KEY_ID_LEN + 32 * 4);
    out.extend_from_slice(ATTRIBUTION_AAD_DOMAIN);
    out.push(block_version);
    out.extend_from_slice(&key_id);
    out.extend_from_slice(compliance_key_digest);
    out.extend_from_slice(e_pk);
    out.extend_from_slice(event_id);
    out.extend_from_slice(recipient);
    Ok(out)
}

/// Canonical `e_pk || SHA-256(P_c)` HKDF salt shared by every attribution verifier.
pub fn attribution_hkdf_salt(e_pk: &[u8; 32], compliance_key_digest: &[u8; 32]) -> [u8; 64] {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(e_pk);
    salt[32..].copy_from_slice(compliance_key_digest);
    salt
}

fn validate_block_version(version: u8) -> Result<(), FormatError> {
    if version != ATTRIBUTION_BLOCK_VERSION {
        return Err(FormatError::UnknownAttributionVersion(version));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    InvalidLength { expected: usize, actual: usize },
    UnknownKeyIdVersion(u8),
    UnknownAttributionVersion(u8),
    UnknownPurpose(u8),
    UnknownJurisdiction(u8),
    NotTracePurpose,
    InvalidTraceEpoch,
    InvalidTraceRetentionPolicy,
    NotAttributionPurpose,
    InvalidAttributionEpoch,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { expected, actual } => {
                write!(f, "invalid length: expected {expected}, got {actual}")
            }
            Self::UnknownKeyIdVersion(value) => write!(f, "unknown key-id version {value}"),
            Self::UnknownAttributionVersion(value) => {
                write!(f, "unknown attribution version {value}")
            }
            Self::UnknownPurpose(value) => write!(f, "unknown compliance purpose {value}"),
            Self::UnknownJurisdiction(value) => write!(f, "unknown jurisdiction {value}"),
            Self::NotTracePurpose => f.write_str("compliance key is not a trace key"),
            Self::InvalidTraceEpoch => f.write_str("invalid daily trace epoch"),
            Self::InvalidTraceRetentionPolicy => {
                f.write_str("invalid jurisdictional trace-retention policy")
            }
            Self::NotAttributionPurpose => f.write_str("compliance key is not an attribution key"),
            Self::InvalidAttributionEpoch => {
                f.write_str("invalid UTC calendar-month attribution epoch")
            }
        }
    }
}

impl std::error::Error for FormatError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_id() -> ComplianceKeyId {
        ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [0xA5; 32],
            0x0102_0304_0506_0708,
            0x090A_0B0C,
        )
    }

    #[test]
    fn key_id_has_one_exact_round_trip() {
        let id = key_id();
        let encoded = id.encode().unwrap();
        assert_eq!(encoded.len(), COMPLIANCE_KEY_ID_LEN);
        assert_eq!(&encoded[..3], &[1, 1, 255]);
        assert_eq!(&encoded[35..43], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&encoded[43..], &[9, 10, 11, 12]);
        assert_eq!(ComplianceKeyId::decode(&encoded), Ok(id));
    }

    #[test]
    fn strict_decoders_reject_lengths_and_unknown_values() {
        assert!(matches!(
            ComplianceKeyId::decode(&[0; COMPLIANCE_KEY_ID_LEN - 1]),
            Err(FormatError::InvalidLength { .. })
        ));
        let mut bytes = key_id().encode().unwrap();
        bytes[0] = 2;
        assert_eq!(
            ComplianceKeyId::decode(&bytes),
            Err(FormatError::UnknownKeyIdVersion(2))
        );
        bytes = key_id().encode().unwrap();
        bytes[1] = 0;
        assert_eq!(
            ComplianceKeyId::decode(&bytes),
            Err(FormatError::UnknownPurpose(0))
        );
        bytes = key_id().encode().unwrap();
        bytes[2] = 4;
        assert_eq!(
            ComplianceKeyId::decode(&bytes),
            Err(FormatError::UnknownJurisdiction(4))
        );
    }

    #[test]
    fn attribution_claim_is_exactly_104_bytes() {
        let claim = AttributionClaim {
            sender_pubkey: [0x11; 32],
            sent_at_ms: 0x0102_0304_0506_0708,
            signature: [0x22; 64],
        };
        let encoded = claim.encode();
        assert_eq!(encoded.len(), ATTRIBUTION_CLAIM_LEN);
        assert_eq!(&encoded[32..40], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(AttributionClaim::decode(&encoded), Ok(claim));
        assert!(AttributionClaim::decode(&encoded[..103]).is_err());
        assert_eq!(format!("{claim:?}"), "AttributionClaim(<withheld>)");
    }

    #[test]
    fn public_context_is_deterministic_and_field_separated_by_fixed_widths() {
        let id = key_id();
        let aad = attribution_aad(3, &id, &[1; 32], &[2; 32], &[3; 32], &[4; 32]).unwrap();
        assert_eq!(
            aad.len(),
            ATTRIBUTION_AAD_DOMAIN.len() + 1 + COMPLIANCE_KEY_ID_LEN + 32 * 4
        );
        let preimage =
            attribution_signing_preimage(3, &id, &[1; 32], &[2; 32], &[3; 32], &[4; 32], 5)
                .unwrap();
        assert_eq!(
            preimage.len(),
            ATTRIBUTION_SIGNATURE_DOMAIN.len() + 1 + COMPLIANCE_KEY_ID_LEN + 32 * 4 + 8
        );
        assert!(attribution_aad(2, &id, &[1; 32], &[2; 32], &[3; 32], &[4; 32]).is_err());
        let salt = attribution_hkdf_salt(&[5; 32], &[6; 32]);
        assert_eq!(&salt[..32], &[5; 32]);
        assert_eq!(&salt[32..], &[6; 32]);
    }

    #[test]
    fn trace_epochs_are_one_exact_aligned_utc_day() {
        for purpose in [
            CompliancePurpose::NetworkTrace,
            CompliancePurpose::IdentityTrace,
        ] {
            let id = ComplianceKeyId::new(
                purpose,
                Jurisdiction::Test,
                [9; 32],
                2 * TRACE_EPOCH_DURATION_MS,
                1,
            );
            let end = 3 * TRACE_EPOCH_DURATION_MS;
            assert_eq!(trace_epoch_end_ms(&id), Ok(end));
            assert_eq!(validate_trace_epoch(&id, id.epoch_start_ms, end), Ok(()));
            assert_eq!(trace_epoch_contains(&id, id.epoch_start_ms), Ok(true));
            assert_eq!(trace_epoch_contains(&id, end - 1), Ok(true));
            assert_eq!(trace_epoch_contains(&id, end), Ok(false));
            assert_eq!(
                validate_trace_epoch(&id, id.epoch_start_ms, end + 1),
                Err(FormatError::InvalidTraceEpoch)
            );
        }

        let unaligned = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [9; 32],
            TRACE_EPOCH_DURATION_MS + 1,
            1,
        );
        assert_eq!(
            trace_epoch_end_ms(&unaligned),
            Err(FormatError::InvalidTraceEpoch)
        );
        let attribution = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [9; 32],
            0,
            1,
        );
        assert_eq!(
            trace_epoch_end_ms(&attribution),
            Err(FormatError::NotTracePurpose)
        );
    }

    #[test]
    fn trace_retention_policy_has_one_shared_utc_runway_calculation() {
        assert_eq!(
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Us,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(30),
            }
            .required_capacity_epochs(),
            Ok(31)
        );
        assert_eq!(
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Tr,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(730),
            }
            .required_capacity_epochs(),
            Ok(731)
        );
        assert_eq!(
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Eu,
                capture: TraceCapturePolicy::Preservation {
                    starts_at_ms: TRACE_EPOCH_DURATION_MS - 1,
                    expires_at_ms: TRACE_EPOCH_DURATION_MS + 1,
                },
                retention_days: None,
            }
            .required_capacity_epochs(),
            Ok(2)
        );
        assert_eq!(
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Test,
                capture: TraceCapturePolicy::Standing,
                retention_days: None,
            }
            .required_capacity_epochs(),
            Ok(1)
        );

        for invalid in [
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Us,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(29),
            },
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Tr,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(731),
            },
            TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Eu,
                capture: TraceCapturePolicy::Preservation {
                    starts_at_ms: 1,
                    expires_at_ms: 1,
                },
                retention_days: None,
            },
        ] {
            assert_eq!(
                invalid.required_capacity_epochs(),
                Err(FormatError::InvalidTraceRetentionPolicy)
            );
        }
    }

    #[test]
    fn attribution_epochs_follow_exact_gregorian_calendar_months() {
        const FEBRUARY_2024: u64 = 1_706_745_600_000;
        const MARCH_2024: u64 = 1_709_251_200_000;
        const FEBRUARY_2023: u64 = 1_675_209_600_000;
        const MARCH_2023: u64 = 1_677_628_800_000;
        const DECEMBER_2023: u64 = 1_701_388_800_000;
        const JANUARY_2024: u64 = 1_704_067_200_000;

        for (start, end) in [
            (FEBRUARY_2024, MARCH_2024),
            (FEBRUARY_2023, MARCH_2023),
            (DECEMBER_2023, JANUARY_2024),
        ] {
            let id = ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [9; 32],
                start,
                1,
            );
            assert_eq!(attribution_epoch_end_ms(&id), Ok(end));
            assert_eq!(compliance_epoch_end_ms(&id), Ok(end));
            assert_eq!(validate_attribution_epoch(&id, start, end), Ok(()));
            assert_eq!(validate_compliance_epoch(&id, start, end), Ok(()));
            assert_eq!(attribution_epoch_contains(&id, start), Ok(true));
            assert_eq!(attribution_epoch_contains(&id, end - 1), Ok(true));
            assert_eq!(attribution_epoch_contains(&id, end), Ok(false));
            assert_eq!(compliance_epoch_contains(&id, end), Ok(false));
        }

        let leap_february = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [9; 32],
            FEBRUARY_2024,
            1,
        );
        assert_eq!(
            validate_attribution_epoch(
                &leap_february,
                FEBRUARY_2024,
                FEBRUARY_2024 + 31 * TRACE_EPOCH_DURATION_MS,
            ),
            Err(FormatError::InvalidAttributionEpoch)
        );
        assert_eq!(
            validate_attribution_epoch(&leap_february, FEBRUARY_2024 + 1, MARCH_2024),
            Err(FormatError::InvalidAttributionEpoch)
        );
    }

    #[test]
    fn attribution_epochs_reject_mid_month_wrong_purpose_and_overflow() {
        const FEBRUARY_2024: u64 = 1_706_745_600_000;
        let mid_month = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [9; 32],
            FEBRUARY_2024 + TRACE_EPOCH_DURATION_MS,
            1,
        );
        assert_eq!(
            attribution_epoch_end_ms(&mid_month),
            Err(FormatError::InvalidAttributionEpoch)
        );
        let trace = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [9; 32],
            FEBRUARY_2024,
            1,
        );
        assert_eq!(
            attribution_epoch_end_ms(&trace),
            Err(FormatError::NotAttributionPurpose)
        );

        let max_day = u64::MAX / TRACE_EPOCH_DURATION_MS;
        let month_start_day = (0..32)
            .map(|back| max_day - back)
            .find(|day| civil_from_days(*day as i64).2 == 1)
            .unwrap();
        let overflow = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [9; 32],
            month_start_day * TRACE_EPOCH_DURATION_MS,
            1,
        );
        assert_eq!(
            attribution_epoch_end_ms(&overflow),
            Err(FormatError::InvalidAttributionEpoch)
        );
    }
}
