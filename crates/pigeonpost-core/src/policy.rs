//! Recipient policy — what a loft is told to enforce on an agent's behalf.
//!
//! A loft cannot see senders, so it cannot apply the per-sender difficulty gradient from
//! `docs/spam.md`. What it *can* do is enforce a flat floor the recipient sets, plus a token gate.
//! The gradient stays client-side, after unwrap.
//!
//! Updates are signed and carry a monotonic `seq`: without it, a captured earlier update could be
//! replayed to re-enable a revoked token or lower the proof-of-work floor.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};

use crate::error::{Error, Result};
use crate::keys::{self, Identity};
use crate::token::Presentation;

const SIG_DOMAIN_POLICY_V1: &[u8] = b"pigeonpost/recipient-policy/v1";
const SIG_DOMAIN_POLICY_V2: &[u8] = b"pigeonpost/recipient-policy/v2";
const SIG_DOMAIN_POLICY_V3: &[u8] = b"pigeonpost/recipient-policy/v3";

/// Policy version emitted by every constructor.
pub const RECIPIENT_POLICY_VERSION: u8 = 3;
/// Authenticated deployed policy version retained for read/migration only.
pub const LEGACY_RECIPIENT_POLICY_VERSION: u8 = 1;
/// Previous authenticated policy version retained for read/migration only.
pub const PREVIOUS_RECIPIENT_POLICY_VERSION: u8 = 2;

/// Version of the canonical recipient attribution requirement.
pub const ATTRIBUTION_REQUIREMENT_VERSION: u8 = 1;
/// Exact byte length of a canonical recipient attribution requirement.
pub const ATTRIBUTION_REQUIREMENT_LEN: usize = 34;

/// Recipient-selected custody scope that both senders and lofts must enforce.
///
/// The authority is stable across monthly key epochs. A sender selects the newest witnessed
/// `Active` attribution key in this exact scope; a loft independently requires the key carried by
/// a new wrap to be `Active` and to match both fields. This prevents the sender from substituting
/// a different jurisdiction or custodian while still preserving scheduled key rotation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttributionRequirement {
    pub version: u8,
    pub jurisdiction: Jurisdiction,
    pub authority: [u8; 32],
}

impl AttributionRequirement {
    pub const fn new(jurisdiction: Jurisdiction, authority: [u8; 32]) -> Self {
        Self {
            version: ATTRIBUTION_REQUIREMENT_VERSION,
            jurisdiction,
            authority,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != ATTRIBUTION_REQUIREMENT_VERSION {
            return Err(Error::MalformedEnvelope(
                "unsupported attribution requirement version",
            ));
        }
        if self.authority == [0u8; 32] {
            return Err(Error::MalformedEnvelope(
                "attribution authority must not be all zeroes",
            ));
        }
        Ok(())
    }

    /// Encode the fixed canonical representation used by signatures and client persistence.
    pub fn encode(&self) -> Result<[u8; ATTRIBUTION_REQUIREMENT_LEN]> {
        self.validate()?;
        let mut encoded = [0u8; ATTRIBUTION_REQUIREMENT_LEN];
        encoded[0] = self.version;
        encoded[1] = self.jurisdiction.into();
        encoded[2..].copy_from_slice(&self.authority);
        Ok(encoded)
    }

    /// Decode an exact canonical representation, rejecting unknown versions and jurisdictions.
    pub fn decode(encoded: &[u8]) -> Result<Self> {
        if encoded.len() != ATTRIBUTION_REQUIREMENT_LEN {
            return Err(Error::MalformedEnvelope(
                "invalid attribution requirement length",
            ));
        }
        if encoded[0] != ATTRIBUTION_REQUIREMENT_VERSION {
            return Err(Error::MalformedEnvelope(
                "unsupported attribution requirement version",
            ));
        }
        let jurisdiction = Jurisdiction::try_from(encoded[1])
            .map_err(|_| Error::MalformedEnvelope("invalid attribution jurisdiction"))?;
        let mut authority = [0u8; 32];
        authority.copy_from_slice(&encoded[2..]);
        let requirement = Self::new(jurisdiction, authority);
        requirement.validate()?;
        Ok(requirement)
    }

    /// Whether an exact compliance key belongs to this recipient-selected scope.
    pub fn matches_key_id(&self, key_id: &ComplianceKeyId) -> bool {
        self.validate().is_ok()
            && key_id.validate().is_ok()
            && key_id.purpose == CompliancePurpose::Attribution
            && key_id.jurisdiction == self.jurisdiction
            && key_id.authority == self.authority
    }
}

/// Cap on live tokens per recipient. Bounded so one agent cannot turn a donated loft's memory
/// into its own storage.
pub const MAX_TOKENS: usize = 64;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecipientPolicy {
    /// Missing on deployed v1 JSON and therefore defaults to the authenticated legacy codec.
    #[serde(default = "legacy_policy_version")]
    pub version: u8,
    pub pubkey: [u8; 32],
    /// Leading zero bits required on unsolicited mail. Zero disables the check.
    pub pow_min: u32,
    /// When true, mail must carry a live capability token.
    pub token_required: bool,
    /// Loft-bound token presentations (`token.rs`), so the loft learns neither tokens nor senders.
    pub token_hashes: Vec<[u8; 32]>,
    /// Compatibility mirror for deployed v1/v2 JSON. Current v3 policies authenticate the exact
    /// requirement below and require this bit to equal `attribution_requirement.is_some()`.
    #[serde(default)]
    pub attribution_required: bool,
    /// Refuse new wraps unless their attribution key is currently Active in this exact recipient-
    /// selected jurisdiction and stable authority scope.
    #[serde(default)]
    pub attribution_requirement: Option<AttributionRequirement>,
    /// Monotonic. A loft rejects any update that does not increase it.
    pub seq: u64,
    #[serde(with = "BigArray")]
    pub signature: [u8; 64],
}

impl core::fmt::Debug for RecipientPolicy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `token_hashes` are the exact loft-bound bearer presentations accepted by this policy.
        // The recipient key and signature also identify the protected inbox, so generic
        // instrumentation gets policy shape only.
        f.debug_struct("RecipientPolicy")
            .field("version", &self.version)
            .field("pow_min", &self.pow_min)
            .field("token_required", &self.token_required)
            .field("token_count", &self.token_hashes.len())
            .field("attribution_required", &self.attribution_required)
            .field("attribution_requirement", &self.attribution_requirement)
            .field("seq", &self.seq)
            .field("protected_material", &"withheld")
            .finish()
    }
}

impl RecipientPolicy {
    pub fn new(
        identity: &Identity,
        pow_min: u32,
        token_required: bool,
        token_hashes: Vec<[u8; 32]>,
        seq: u64,
    ) -> Self {
        Self::with_attribution_requirement(
            identity,
            pow_min,
            token_required,
            token_hashes,
            seq,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_attribution_requirement(
        identity: &Identity,
        pow_min: u32,
        token_required: bool,
        token_hashes: Vec<[u8; 32]>,
        seq: u64,
        attribution_requirement: Option<AttributionRequirement>,
    ) -> Self {
        let pubkey = identity.verifying_key().to_bytes();
        let payload = payload_v3(
            &pubkey,
            pow_min,
            token_required,
            &token_hashes,
            seq,
            attribution_requirement.as_ref(),
        );
        RecipientPolicy {
            version: RECIPIENT_POLICY_VERSION,
            pubkey,
            pow_min,
            token_required,
            token_hashes,
            seq,
            attribution_required: attribution_requirement.is_some(),
            attribution_requirement,
            signature: identity.sign(&payload).to_bytes(),
        }
    }

    /// The default posture for an agent that has said nothing: accept mail, require no work.
    ///
    /// `acceptAll = false` from `docs/spam.md` lives in the *client*, which queues unknown
    /// senders as pending after unwrap. A loft cannot implement it, because it cannot see who
    /// sent anything.
    pub fn permissive(pubkey: [u8; 32]) -> Self {
        RecipientPolicy {
            version: RECIPIENT_POLICY_VERSION,
            pubkey,
            pow_min: 0,
            token_required: false,
            token_hashes: Vec::new(),
            seq: 0,
            attribution_required: false,
            attribution_requirement: None,
            signature: [0u8; 64],
        }
    }

    /// Verify the signature and that `seq` advances past what the loft already stored.
    pub fn verify(&self, last_seq: Option<u64>) -> Result<VerifyingKey> {
        if self.token_hashes.len() > MAX_TOKENS {
            return Err(Error::TooLarge);
        }
        if let Some(last) = last_seq {
            if self.seq <= last {
                return Err(Error::StaleSequence);
            }
        }

        let key = keys::verifying_key_from_bytes(&self.pubkey)?;
        let payload = match self.version {
            LEGACY_RECIPIENT_POLICY_VERSION if self.attribution_requirement.is_none() => {
                payload_v1(
                    &self.pubkey,
                    self.pow_min,
                    self.token_required,
                    &self.token_hashes,
                    self.seq,
                    self.attribution_required,
                )
            }
            PREVIOUS_RECIPIENT_POLICY_VERSION if self.attribution_requirement.is_none() => {
                payload_v2(
                    &self.pubkey,
                    self.pow_min,
                    self.token_required,
                    &self.token_hashes,
                    self.seq,
                    self.attribution_required,
                )
            }
            RECIPIENT_POLICY_VERSION
                if self.attribution_required == self.attribution_requirement.is_some() =>
            {
                if let Some(requirement) = self.attribution_requirement {
                    requirement.validate()?;
                }
                payload_v3(
                    &self.pubkey,
                    self.pow_min,
                    self.token_required,
                    &self.token_hashes,
                    self.seq,
                    self.attribution_requirement.as_ref(),
                )
            }
            LEGACY_RECIPIENT_POLICY_VERSION | PREVIOUS_RECIPIENT_POLICY_VERSION => {
                return Err(Error::MalformedEnvelope(
                    "legacy policy carries unsigned attribution scope",
                ));
            }
            RECIPIENT_POLICY_VERSION => {
                return Err(Error::MalformedEnvelope(
                    "inconsistent attribution requirement",
                ));
            }
            _ => return Err(Error::MalformedEnvelope("unsupported policy version")),
        };
        keys::verify(&key, &payload, &Signature::from_bytes(&self.signature))?;
        Ok(key)
    }

    /// Verify an authenticated v1/v2 policy and re-sign it as v3 at a higher sequence.
    ///
    /// A legacy required bit did not authenticate jurisdiction or authority. Callers must supply
    /// that missing choice explicitly; silently guessing would preserve the original vulnerability.
    pub fn migrate_legacy(
        &self,
        identity: &Identity,
        next_seq: u64,
        attribution_requirement: Option<AttributionRequirement>,
    ) -> Result<Self> {
        if !matches!(
            self.version,
            LEGACY_RECIPIENT_POLICY_VERSION | PREVIOUS_RECIPIENT_POLICY_VERSION
        ) {
            return Err(Error::MalformedEnvelope("policy is not legacy"));
        }
        self.verify(None)?;
        if identity.verifying_key().to_bytes() != self.pubkey {
            return Err(Error::InvalidKey);
        }
        if next_seq <= self.seq {
            return Err(Error::StaleSequence);
        }
        if self.attribution_required && attribution_requirement.is_none() {
            return Err(Error::MalformedEnvelope(
                "legacy attribution policy requires an explicit scope",
            ));
        }
        if let Some(requirement) = attribution_requirement {
            requirement.validate()?;
        }
        Ok(Self::with_attribution_requirement(
            identity,
            self.pow_min,
            self.token_required,
            self.token_hashes.clone(),
            next_seq,
            attribution_requirement,
        ))
    }

    /// Whether a presented token is currently live for this recipient. Constant-time per entry.
    pub fn accepts_token(&self, presented: &Presentation) -> bool {
        self.token_hashes
            .iter()
            .any(|registered| presented.matches(&Presentation::from_bytes(*registered)))
    }
}

/// The signed preimage.
///
/// Built field by field, so **every new field must be added here too**. A field outside this
/// payload is a field a loft can flip on a policy it is merely holding — which for
/// `attribution_required` would mean silently switching the gate off.
fn payload_v1(
    pubkey: &[u8; 32],
    pow_min: u32,
    token_required: bool,
    token_hashes: &[[u8; 32]],
    seq: u64,
    attribution_required: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(SIG_DOMAIN_POLICY_V1.len() + 64 + token_hashes.len() * 32);
    out.extend_from_slice(SIG_DOMAIN_POLICY_V1);
    out.extend_from_slice(pubkey);
    out.extend_from_slice(&pow_min.to_le_bytes());
    out.push(u8::from(token_required));
    out.extend_from_slice(&(token_hashes.len() as u32).to_le_bytes());
    for hash in token_hashes {
        out.extend_from_slice(hash);
    }
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(u8::from(attribution_required));
    out
}

fn payload_v2(
    pubkey: &[u8; 32],
    pow_min: u32,
    token_required: bool,
    token_hashes: &[[u8; 32]],
    seq: u64,
    attribution_required: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(SIG_DOMAIN_POLICY_V2.len() + 65 + token_hashes.len() * 32);
    out.extend_from_slice(SIG_DOMAIN_POLICY_V2);
    out.push(PREVIOUS_RECIPIENT_POLICY_VERSION);
    out.extend_from_slice(pubkey);
    out.extend_from_slice(&pow_min.to_be_bytes());
    out.push(u8::from(token_required));
    out.extend_from_slice(&(token_hashes.len() as u32).to_be_bytes());
    for hash in token_hashes {
        out.extend_from_slice(hash);
    }
    out.push(u8::from(attribution_required));
    out.extend_from_slice(&seq.to_be_bytes());
    out
}

fn payload_v3(
    pubkey: &[u8; 32],
    pow_min: u32,
    token_required: bool,
    token_hashes: &[[u8; 32]],
    seq: u64,
    attribution_requirement: Option<&AttributionRequirement>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        SIG_DOMAIN_POLICY_V3.len() + 65 + token_hashes.len() * 32 + ATTRIBUTION_REQUIREMENT_LEN,
    );
    out.extend_from_slice(SIG_DOMAIN_POLICY_V3);
    out.push(RECIPIENT_POLICY_VERSION);
    out.extend_from_slice(pubkey);
    out.extend_from_slice(&pow_min.to_be_bytes());
    out.push(u8::from(token_required));
    out.extend_from_slice(&(token_hashes.len() as u32).to_be_bytes());
    for hash in token_hashes {
        out.extend_from_slice(hash);
    }
    match attribution_requirement {
        Some(requirement) => {
            out.push(1);
            out.push(requirement.version);
            out.push(requirement.jurisdiction.into());
            out.extend_from_slice(&requirement.authority);
        }
        None => out.push(0),
    }
    out.extend_from_slice(&seq.to_be_bytes());
    out
}

const fn legacy_policy_version() -> u8 {
    LEGACY_RECIPIENT_POLICY_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    const LOFT: [u8; 32] = [9; 32];

    fn requirement() -> AttributionRequirement {
        AttributionRequirement::new(Jurisdiction::Test, [0xA5; 32])
    }

    fn legacy_policy(identity: &Identity, seq: u64) -> RecipientPolicy {
        let pubkey = identity.verifying_key().to_bytes();
        let token_hashes = vec![[7; 32]];
        let signature = identity
            .sign(&payload_v1(&pubkey, 17, true, &token_hashes, seq, true))
            .to_bytes();
        RecipientPolicy {
            version: LEGACY_RECIPIENT_POLICY_VERSION,
            pubkey,
            pow_min: 17,
            token_required: true,
            token_hashes,
            attribution_required: true,
            attribution_requirement: None,
            seq,
            signature,
        }
    }

    #[test]
    fn a_signed_policy_verifies() {
        let agent = Identity::from_seed([1; 32]);
        let policy = RecipientPolicy::new(&agent, 18, false, vec![], 1);
        assert_eq!(policy.version, RECIPIENT_POLICY_VERSION);
        assert_eq!(policy.verify(None).unwrap(), agent.verifying_key());
    }

    #[test]
    fn policy_debug_withholds_recipient_and_bearer_presentations() {
        let agent = Identity::from_seed([0xA7; 32]);
        let presentation = Token::mint(&[0xB8; 32], "policy-debug-canary")
            .presentation(&LOFT, "https://loft.example")
            .unwrap();
        let policy = RecipientPolicy::new(&agent, 18, true, vec![*presentation.as_bytes()], 4_567);
        let debugged = format!("{policy:?}");

        assert_eq!(
            debugged,
            "RecipientPolicy { version: 3, pow_min: 18, token_required: true, token_count: 1, attribution_required: false, attribution_requirement: None, seq: 4567, protected_material: \"withheld\" }"
        );
        assert!(!debugged.contains("policy-debug-canary"));
    }

    #[test]
    fn authenticated_v1_without_a_version_field_remains_readable() {
        let agent = Identity::from_seed([1; 32]);
        let legacy = legacy_policy(&agent, 8);
        let mut json = serde_json::to_value(&legacy).unwrap();
        json.as_object_mut().unwrap().remove("version");

        let decoded: RecipientPolicy = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.version, LEGACY_RECIPIENT_POLICY_VERSION);
        assert_eq!(decoded.verify(None).unwrap(), agent.verifying_key());
    }

    #[test]
    fn legacy_migration_requires_owner_higher_sequence_and_explicit_scope() {
        let agent = Identity::from_seed([1; 32]);
        let attacker = Identity::from_seed([2; 32]);
        let legacy = legacy_policy(&agent, 8);

        assert_eq!(
            legacy.migrate_legacy(&attacker, 9, Some(requirement())),
            Err(Error::InvalidKey)
        );
        assert_eq!(
            legacy.migrate_legacy(&agent, 8, Some(requirement())),
            Err(Error::StaleSequence)
        );
        assert_eq!(
            legacy.migrate_legacy(&agent, 9, None),
            Err(Error::MalformedEnvelope(
                "legacy attribution policy requires an explicit scope"
            ))
        );
        let migrated = legacy
            .migrate_legacy(&agent, 9, Some(requirement()))
            .unwrap();
        assert_eq!(migrated.version, RECIPIENT_POLICY_VERSION);
        assert_eq!(migrated.pow_min, legacy.pow_min);
        assert_eq!(migrated.token_hashes, legacy.token_hashes);
        assert!(migrated.attribution_required);
        assert_eq!(migrated.attribution_requirement, Some(requirement()));
        assert!(migrated.verify(Some(8)).is_ok());
    }

    #[test]
    fn policy_versions_and_unknown_json_fail_closed() {
        let agent = Identity::from_seed([1; 32]);
        let mut policy = RecipientPolicy::new(&agent, 18, false, vec![], 1);
        policy.version = LEGACY_RECIPIENT_POLICY_VERSION;
        assert_eq!(policy.verify(None), Err(Error::BadSignature));

        policy.version = 9;
        assert_eq!(
            policy.verify(None),
            Err(Error::MalformedEnvelope("unsupported policy version"))
        );

        let mut json =
            serde_json::to_value(RecipientPolicy::new(&agent, 18, false, vec![], 1)).unwrap();
        json["unsigned_future_field"] = serde_json::json!(true);
        assert!(serde_json::from_value::<RecipientPolicy>(json).is_err());
    }

    #[test]
    fn v3_signing_codec_is_pinned() {
        let payload = payload_v3(
            &[1; 32],
            0x0102_0304,
            true,
            &[[2; 32]],
            0x0506_0708_090a_0b0c,
            Some(&requirement()),
        );
        let encoded: String = payload.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            encoded,
            "706967656f6e706f73742f726563697069656e742d706f6c6963792f763303010101010101010101010101010101010101010101010101010101010101010101020304010000000102020202020202020202020202020202020202020202020202020202020202020101ffa5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a505060708090a0b0c"
        );
    }

    #[test]
    fn deployed_v2_payload_and_signature_remain_pinned() {
        let payload = payload_v2(
            &[1; 32],
            0x0102_0304,
            true,
            &[[2; 32]],
            0x0506_0708_090a_0b0c,
            true,
        );
        let encoded: String = payload.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            encoded,
            "706967656f6e706f73742f726563697069656e742d706f6c6963792f763202010101010101010101010101010101010101010101010101010101010101010101020304010000000102020202020202020202020202020202020202020202020202020202020202020105060708090a0b0c"
        );
        assert_eq!(
            Identity::from_seed([1; 32]).sign(&payload).to_bytes(),
            [
                80, 240, 252, 21, 179, 27, 57, 213, 207, 180, 181, 132, 43, 190, 205, 84, 41, 218,
                148, 216, 129, 189, 19, 96, 126, 89, 63, 140, 134, 192, 235, 255, 200, 84, 201,
                113, 143, 187, 255, 128, 228, 127, 100, 229, 175, 176, 2, 74, 95, 249, 35, 227,
                138, 141, 12, 1, 127, 17, 27, 205, 38, 255, 115, 5,
            ]
        );

        let owner = Identity::from_seed([1; 32]);
        let owner_key = owner.verifying_key().to_bytes();
        let fixture_payload = payload_v2(&owner_key, 17, true, &[[7; 32]], 8, true);
        let fixture_signature = [
            191, 117, 62, 1, 186, 69, 74, 48, 207, 115, 106, 192, 254, 135, 87, 39, 147, 41, 151,
            188, 201, 185, 216, 228, 70, 194, 190, 248, 211, 162, 139, 136, 44, 123, 85, 169, 249,
            139, 79, 77, 132, 229, 162, 31, 217, 27, 35, 41, 166, 76, 252, 182, 117, 157, 244, 164,
            226, 212, 15, 170, 209, 12, 222, 2,
        ];
        assert_eq!(owner.sign(&fixture_payload).to_bytes(), fixture_signature);
        let fixture = RecipientPolicy {
            version: PREVIOUS_RECIPIENT_POLICY_VERSION,
            pubkey: owner_key,
            pow_min: 17,
            token_required: true,
            token_hashes: vec![[7; 32]],
            attribution_required: true,
            attribution_requirement: None,
            seq: 8,
            signature: fixture_signature,
        };
        assert_eq!(fixture.verify(None).unwrap(), owner.verifying_key());
    }

    #[test]
    fn only_the_owner_can_set_policy() {
        let agent = Identity::from_seed([1; 32]);
        let attacker = Identity::from_seed([2; 32]);

        let mut forged = RecipientPolicy::new(&attacker, 0, false, vec![], 1);
        forged.pubkey = agent.verifying_key().to_bytes();

        assert_eq!(forged.verify(None), Err(Error::BadSignature));
    }

    #[test]
    fn replaying_an_earlier_policy_is_refused() {
        // The attack: capture the update that had a token live, replay it after revocation.
        let agent = Identity::from_seed([1; 32]);
        let old = RecipientPolicy::new(&agent, 0, true, vec![[1u8; 32]], 5);

        assert!(old.verify(Some(4)).is_ok());
        assert_eq!(old.verify(Some(5)), Err(Error::StaleSequence));
        assert_eq!(old.verify(Some(6)), Err(Error::StaleSequence));
    }

    #[test]
    fn a_loft_cannot_switch_off_the_attribution_gate() {
        // The failure this guards: a loft holding a signed policy flips one bool and the
        // requirement quietly disappears.
        let agent = Identity::from_seed([1; 32]);
        let mut policy = RecipientPolicy::with_attribution_requirement(
            &agent,
            0,
            false,
            vec![],
            1,
            Some(requirement()),
        );
        assert!(policy.verify(None).is_ok());

        policy.attribution_required = false;
        assert_eq!(
            policy.verify(None),
            Err(Error::MalformedEnvelope(
                "inconsistent attribution requirement"
            ))
        );
    }

    #[test]
    fn requirement_scope_and_version_are_authenticated() {
        let agent = Identity::from_seed([1; 32]);
        let mut policy = RecipientPolicy::with_attribution_requirement(
            &agent,
            0,
            false,
            vec![],
            1,
            Some(requirement()),
        );
        policy.attribution_requirement.as_mut().unwrap().authority[0] ^= 1;
        assert_eq!(policy.verify(None), Err(Error::BadSignature));

        let mut policy = RecipientPolicy::with_attribution_requirement(
            &agent,
            0,
            false,
            vec![],
            1,
            Some(requirement()),
        );
        policy.attribution_requirement.as_mut().unwrap().version = 2;
        assert_eq!(
            policy.verify(None),
            Err(Error::MalformedEnvelope(
                "unsupported attribution requirement version"
            ))
        );
    }

    #[test]
    fn canonical_requirement_matches_only_exact_attribution_scope() {
        let requirement = requirement();
        let encoded = requirement.encode().unwrap();
        assert_eq!(AttributionRequirement::decode(&encoded), Ok(requirement));

        let matching = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [0xA5; 32],
            1_785_542_400_000,
            0,
        );
        assert!(requirement.matches_key_id(&matching));

        let wrong_authority = ComplianceKeyId {
            authority: [0xB6; 32],
            ..matching
        };
        assert!(!requirement.matches_key_id(&wrong_authority));
        assert_eq!(
            AttributionRequirement::new(Jurisdiction::Test, [0; 32]).validate(),
            Err(Error::MalformedEnvelope(
                "attribution authority must not be all zeroes"
            ))
        );
        let mut zero_authority = encoded;
        zero_authority[2..].fill(0);
        assert_eq!(
            AttributionRequirement::decode(&zero_authority),
            Err(Error::MalformedEnvelope(
                "attribution authority must not be all zeroes"
            ))
        );
    }

    #[test]
    fn lowering_the_pow_floor_requires_a_fresh_signature() {
        let agent = Identity::from_seed([1; 32]);
        let mut policy = RecipientPolicy::new(&agent, 20, false, vec![], 1);
        policy.pow_min = 0;

        assert_eq!(policy.verify(None), Err(Error::BadSignature));
    }

    #[test]
    fn token_gate_accepts_only_registered_presentations() {
        let agent = Identity::from_seed([1; 32]);
        let good = Token::mint(&[7; 32], "readme")
            .presentation(&LOFT, "https://loft.example")
            .unwrap();
        let bad = Token::mint(&[7; 32], "revoked")
            .presentation(&LOFT, "https://loft.example")
            .unwrap();

        let policy = RecipientPolicy::new(&agent, 0, true, vec![*good.as_bytes()], 1);

        assert!(policy.accepts_token(&good));
        assert!(!policy.accepts_token(&bad));
    }

    #[test]
    fn too_many_tokens_is_refused() {
        let agent = Identity::from_seed([1; 32]);
        let hashes = vec![[0u8; 32]; MAX_TOKENS + 1];
        let policy = RecipientPolicy::new(&agent, 0, true, hashes, 1);

        assert_eq!(policy.verify(None), Err(Error::TooLarge));
    }

    #[test]
    fn the_default_posture_is_permissive_at_the_loft() {
        let agent = Identity::from_seed([1; 32]);
        let policy = RecipientPolicy::permissive(agent.verifying_key().to_bytes());
        assert_eq!(policy.pow_min, 0);
        assert!(!policy.token_required);
    }
}
