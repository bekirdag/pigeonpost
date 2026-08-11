//! Agent records and rotation records.
//!
//! An agent record is what a sender fetches to learn a key address's public key and lofts. It is
//! self-verifying — anyone can check `SHA-256(pubkey)` against the address and check the
//! signature — which is why serving one confers no authority and any loft, mirror, or CDN can do
//! it (`docs/architecture.md`).
//!
//! Rotation is the mechanism from `docs/keys.md`: every record commits to a successor *before*
//! it is needed, so a rotation can only ever go to the key that was committed to.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::address::Address;
use crate::error::{Error, Result};
use crate::keys::{self, Identity, SuccessorCommitment};
use crate::network::{is_localhost_name, is_numeric_loopback_host};
use crate::policy::AttributionRequirement;

const SIG_DOMAIN_RECORD_V1: &[u8] = b"pigeonpost/agent-record/v1";
const SIG_DOMAIN_RECORD_V2: &[u8] = b"pigeonpost/agent-record/v2";
const SIG_DOMAIN_ROTATION_OUTGOING: &[u8] = b"pigeonpost/rotation/v2/outgoing";
const SIG_DOMAIN_ROTATION_INCOMING: &[u8] = b"pigeonpost/rotation/v2/incoming";

/// Current signed rotation-record format. The deployed v1 prototype was outgoing-key-only and is
/// deliberately not accepted: a compromised outgoing key could choose the incoming key's next
/// commitment and poison the following transition.
pub const ROTATION_RECORD_VERSION: u8 = 2;
/// Retired keys remain available for dual-address drain for exactly 90 days.
pub const ROTATION_GRACE_SECS: u64 = 90 * 24 * 60 * 60;
/// A transition may be published shortly before another machine's wall clock catches up.
pub const ROTATION_CLOCK_SKEW_SECS: u64 = 5 * 60;
/// A record normally publishes three lofts. A small transition allowance keeps rotation and
/// operator-driven replacement possible without letting a recipient turn resolution into an
/// unbounded fan-out.
pub const MAX_AGENT_RECORD_LOFTS: usize = 8;
/// Individual service origins share the same wire bound as explicit address hints.
pub const MAX_AGENT_RECORD_LOFT_URL_BYTES: usize = 2_048;
/// The complete signed routing set remains comfortably below the bounded control-response budget.
pub const MAX_AGENT_RECORD_LOFT_BYTES: usize =
    MAX_AGENT_RECORD_LOFTS * MAX_AGENT_RECORD_LOFT_URL_BYTES;
/// Agent-record version emitted by every constructor.
pub const AGENT_RECORD_VERSION: u8 = 2;
/// Deployed record version retained for authenticated reads and explicit migration.
pub const LEGACY_AGENT_RECORD_VERSION: u8 = 1;

/// Published at an agent's address. Cacheable by anyone, trusted from nobody.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRecord {
    /// Missing on deployed v1 JSON and therefore defaults to the authenticated legacy codec.
    #[serde(default = "legacy_agent_record_version")]
    pub version: u8,
    pub pubkey: [u8; 32],
    /// `SHA-256` of the key this agent will rotate to. Set at creation; an agent that omits it
    /// can never rotate.
    pub successor_hash: [u8; 32],
    /// Monotonic. Rejecting non-increasing values is what stops an old record being replayed to
    /// undo a rotation.
    pub seq: u64,
    pub lofts: Vec<String>,
    /// Proof-of-work this agent's lofts will demand of unsolicited mail.
    ///
    /// Advertised here because the sender already fetches this record, and because a floor the
    /// sender cannot see in advance would mean mining blind or a rejected round trip
    /// (`docs/spam.md`). Signed, so a loft cannot inflate it to price someone out.
    #[serde(default)]
    pub pow_min: u32,
    /// Recipient-signed jurisdiction and stable custody authority required for attribution.
    #[serde(default)]
    pub attribution_requirement: Option<AttributionRequirement>,
    #[serde(with = "serde_big_array::BigArray")]
    pub signature: [u8; 64],
}

impl AgentRecord {
    pub fn new(
        identity: &Identity,
        successor: &SuccessorCommitment,
        seq: u64,
        lofts: Vec<String>,
    ) -> Self {
        Self::with_policy(identity, successor, seq, lofts, 0, None)
    }

    /// Publish a record that also advertises a proof-of-work floor.
    pub fn with_pow(
        identity: &Identity,
        successor: &SuccessorCommitment,
        seq: u64,
        lofts: Vec<String>,
        pow_min: u32,
    ) -> Self {
        Self::with_policy(identity, successor, seq, lofts, pow_min, None)
    }

    /// Publish routing and the exact attribution scope a sender must explicitly agree to use.
    pub fn with_policy(
        identity: &Identity,
        successor: &SuccessorCommitment,
        seq: u64,
        lofts: Vec<String>,
        pow_min: u32,
        attribution_requirement: Option<AttributionRequirement>,
    ) -> Self {
        let payload = record_payload_v2(
            &identity.verifying_key().to_bytes(),
            successor.as_bytes(),
            seq,
            &lofts,
            pow_min,
            attribution_requirement.as_ref(),
        );
        let signature = identity.sign(&payload).to_bytes();
        AgentRecord {
            version: AGENT_RECORD_VERSION,
            pubkey: identity.verifying_key().to_bytes(),
            successor_hash: *successor.as_bytes(),
            seq,
            lofts,
            pow_min,
            attribution_requirement,
            signature,
        }
    }

    /// Verify the signature and that this record belongs to `address`.
    ///
    /// Both halves matter: the signature proves the record was not altered, and the address check
    /// proves it is the record for the address that was asked about — without it, a hostile
    /// directory could answer with a perfectly valid record for a *different* agent.
    pub fn verify(&self, address: &Address) -> Result<VerifyingKey> {
        let pubkey = keys::verifying_key_from_bytes(&self.pubkey)?;
        if !address.matches(&pubkey) {
            return Err(Error::MalformedAddress("record is for a different address"));
        }
        self.validate_lofts()?;
        let payload = match self.version {
            LEGACY_AGENT_RECORD_VERSION if self.attribution_requirement.is_none() => {
                record_payload_v1(
                    &self.pubkey,
                    &self.successor_hash,
                    self.seq,
                    &self.lofts,
                    self.pow_min,
                )
            }
            AGENT_RECORD_VERSION => {
                if let Some(requirement) = self.attribution_requirement {
                    requirement.validate()?;
                }
                record_payload_v2(
                    &self.pubkey,
                    &self.successor_hash,
                    self.seq,
                    &self.lofts,
                    self.pow_min,
                    self.attribution_requirement.as_ref(),
                )
            }
            LEGACY_AGENT_RECORD_VERSION => {
                return Err(Error::MalformedEnvelope(
                    "legacy agent record carries unsigned attribution scope",
                ));
            }
            _ => {
                return Err(Error::MalformedEnvelope("unsupported agent record version"));
            }
        };
        keys::verify(&pubkey, &payload, &Signature::from_bytes(&self.signature))?;
        Ok(pubkey)
    }

    /// Verify an authenticated v1 record and re-sign its routing as v2 at a higher sequence.
    pub fn migrate_v1(
        &self,
        identity: &Identity,
        next_seq: u64,
        attribution_requirement: Option<AttributionRequirement>,
    ) -> Result<Self> {
        if self.version != LEGACY_AGENT_RECORD_VERSION {
            return Err(Error::MalformedEnvelope("agent record is not v1"));
        }
        let owner = keys::verifying_key_from_bytes(&self.pubkey)?;
        self.verify(&Address::from_pubkey(&owner))?;
        if identity.verifying_key().to_bytes() != self.pubkey {
            return Err(Error::InvalidKey);
        }
        if next_seq <= self.seq {
            return Err(Error::StaleSequence);
        }
        if let Some(requirement) = attribution_requirement {
            requirement.validate()?;
        }
        Ok(Self::with_policy(
            identity,
            &self.successor_commitment(),
            next_seq,
            self.lofts.clone(),
            self.pow_min,
            attribution_requirement,
        ))
    }

    /// Validate the signed routing set before it is stored or used for network requests.
    ///
    /// This is intentionally a synchronous syntax/size gate. A network client must additionally
    /// resolve every hostname, reject non-public addresses, and pin the validated connection.
    /// Exact numeric loopback HTTP remains representable for explicit local installations, but a
    /// network-sourced client must not grant it local trust merely because the record is signed.
    pub fn validate_lofts(&self) -> Result<()> {
        validate_loft_list(&self.lofts)
    }

    pub fn successor_commitment(&self) -> SuccessorCommitment {
        SuccessorCommitment(self.successor_hash)
    }
}

/// Validate a routing list before constructing a signed record in a stateful client.
pub fn validate_loft_list(lofts: &[String]) -> Result<()> {
    if lofts.len() > MAX_AGENT_RECORD_LOFTS {
        return Err(Error::TooLarge);
    }

    let mut total = 0usize;
    for loft in lofts {
        if loft.is_empty() || loft.len() > MAX_AGENT_RECORD_LOFT_URL_BYTES {
            return Err(Error::TooLarge);
        }
        total = total.checked_add(loft.len()).ok_or(Error::TooLarge)?;
        if total > MAX_AGENT_RECORD_LOFT_BYTES {
            return Err(Error::TooLarge);
        }

        let url = Url::parse(loft)
            .map_err(|_| Error::MalformedEnvelope("agent record loft origin is invalid"))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
            || url.host_str().is_none()
            || url.port() == Some(0)
        {
            return Err(Error::MalformedEnvelope(
                "agent record loft origin is invalid",
            ));
        }

        let host = url.host_str().ok_or(Error::MalformedEnvelope(
            "agent record loft origin is invalid",
        ))?;
        if is_localhost_name(host) {
            return Err(Error::MalformedEnvelope(
                "agent record loft origin is invalid",
            ));
        }
        let numeric_loopback = is_numeric_loopback_host(host);
        if url.scheme() != "https" && !(url.scheme() == "http" && numeric_loopback) {
            return Err(Error::MalformedEnvelope(
                "agent record loft origin is invalid",
            ));
        }
    }
    Ok(())
}

/// Moves an address from one key to its precommitted successor.
///
/// Both keys sign the complete transition. The outgoing signature proves continuity from the old
/// address; the incoming signature proves that the precommitted successor chose its own next
/// commitment. Requiring both prevents a compromised outgoing key from poisoning the chain after
/// forcing the one transition it was already authorized to make.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RotationRecord {
    pub version: u8,
    pub from_pubkey: [u8; 32],
    pub to_pubkey: [u8; 32],
    /// Commitment to the key *after* this one — the chain continues, or rotation ends here.
    pub next_successor_hash: [u8; 32],
    pub seq: u64,
    /// Unix seconds at which senders may follow the transition.
    pub activated_at: u64,
    /// Exclusive end of the retired-key dual-drain window.
    pub grace_until: u64,
    #[serde(with = "serde_big_array::BigArray")]
    pub outgoing_signature: [u8; 64],
    #[serde(with = "serde_big_array::BigArray")]
    pub incoming_signature: [u8; 64],
}

impl RotationRecord {
    pub fn new(
        outgoing: &Identity,
        incoming: &Identity,
        next_successor: &SuccessorCommitment,
        seq: u64,
        activated_at: u64,
    ) -> Result<Self> {
        let grace_until = activated_at
            .checked_add(ROTATION_GRACE_SECS)
            .ok_or(Error::MalformedEnvelope("invalid rotation timing"))?;
        if activated_at == 0 {
            return Err(Error::MalformedEnvelope("invalid rotation timing"));
        }
        let body = rotation_body(
            ROTATION_RECORD_VERSION,
            &outgoing.verifying_key().to_bytes(),
            &incoming.verifying_key().to_bytes(),
            next_successor.as_bytes(),
            seq,
            activated_at,
            grace_until,
        );
        let record = RotationRecord {
            version: ROTATION_RECORD_VERSION,
            from_pubkey: outgoing.verifying_key().to_bytes(),
            to_pubkey: incoming.verifying_key().to_bytes(),
            next_successor_hash: *next_successor.as_bytes(),
            seq,
            activated_at,
            grace_until,
            outgoing_signature: outgoing
                .sign(&rotation_signing_payload(
                    SIG_DOMAIN_ROTATION_OUTGOING,
                    &body,
                ))
                .to_bytes(),
            incoming_signature: incoming
                .sign(&rotation_signing_payload(
                    SIG_DOMAIN_ROTATION_INCOMING,
                    &body,
                ))
                .to_bytes(),
        };
        Ok(record)
    }

    /// Verify a rotation against what we already know about this address.
    ///
    /// Three checks, each closing a specific attack:
    /// 1. the signatures are by both the outgoing and incoming keys;
    /// 2. the target matches the **pinned** successor commitment — a compromised key cannot
    ///    rotate to a key of the attacker's choosing;
    /// 3. `seq` advances exactly once — a replay or skipped transition cannot be accepted;
    /// 4. activation and the exact 90-day grace interval are signed and structurally valid.
    ///
    /// Historical records remain verifiable after `grace_until`: expiry ends retired-key drain,
    /// not the ability of a sender waking up later to learn the current address.
    pub fn verify(
        &self,
        pinned: &SuccessorCommitment,
        last_seq: u64,
        observed_at: u64,
    ) -> Result<VerifiedRotation> {
        if self.version != ROTATION_RECORD_VERSION
            || self.activated_at == 0
            || self.grace_until.checked_sub(self.activated_at) != Some(ROTATION_GRACE_SECS)
        {
            return Err(Error::MalformedEnvelope("invalid rotation record"));
        }
        let from = keys::verifying_key_from_bytes(&self.from_pubkey)?;
        let to = keys::verifying_key_from_bytes(&self.to_pubkey)?;

        let body = rotation_body(
            self.version,
            &self.from_pubkey,
            &self.to_pubkey,
            &self.next_successor_hash,
            self.seq,
            self.activated_at,
            self.grace_until,
        );
        keys::verify(
            &from,
            &rotation_signing_payload(SIG_DOMAIN_ROTATION_OUTGOING, &body),
            &Signature::from_bytes(&self.outgoing_signature),
        )?;
        keys::verify(
            &to,
            &rotation_signing_payload(SIG_DOMAIN_ROTATION_INCOMING, &body),
            &Signature::from_bytes(&self.incoming_signature),
        )?;

        if !pinned.accepts(&to) {
            return Err(Error::SuccessorMismatch);
        }
        if last_seq.checked_add(1) != Some(self.seq) {
            return Err(Error::StaleSequence);
        }
        if observed_at.saturating_add(ROTATION_CLOCK_SKEW_SECS) < self.activated_at {
            return Err(Error::StaleTimestamp);
        }

        Ok(VerifiedRotation {
            incoming: to,
            next_successor: SuccessorCommitment(self.next_successor_hash),
            activated_at: self.activated_at,
            grace_until: self.grace_until,
        })
    }

    /// Verify that a routed record really starts at the requested address before consulting its
    /// signatures or successor pin.
    pub fn verify_source_address(&self, address: &Address) -> Result<()> {
        let from = keys::verifying_key_from_bytes(&self.from_pubkey)?;
        if !address.matches(&from) {
            return Err(Error::MalformedAddress(
                "rotation is for a different address",
            ));
        }
        Ok(())
    }

    pub fn target_address(&self) -> Result<Address> {
        let to = keys::verifying_key_from_bytes(&self.to_pubkey)?;
        Ok(Address::from_pubkey(&to))
    }

    pub fn retired_key_is_active(&self, now: u64) -> bool {
        now >= self.activated_at && now < self.grace_until
    }
}

/// Authenticated transition state returned only after both signatures and the pinned commitment
/// have verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRotation {
    pub incoming: VerifyingKey,
    pub next_successor: SuccessorCommitment,
    pub activated_at: u64,
    pub grace_until: u64,
}

fn record_payload_v1(
    pubkey: &[u8; 32],
    successor: &[u8; 32],
    seq: u64,
    lofts: &[String],
    pow_min: u32,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(SIG_DOMAIN_RECORD_V1.len() + 72 + lofts.len() * 32);
    payload.extend_from_slice(SIG_DOMAIN_RECORD_V1);
    payload.extend_from_slice(pubkey);
    payload.extend_from_slice(successor);
    payload.extend_from_slice(&seq.to_le_bytes());
    // Length-prefix each loft so ["ab","c"] cannot collide with ["a","bc"].
    payload.extend_from_slice(&(lofts.len() as u32).to_le_bytes());
    for loft in lofts {
        payload.extend_from_slice(&(loft.len() as u32).to_le_bytes());
        payload.extend_from_slice(loft.as_bytes());
    }
    payload.extend_from_slice(&pow_min.to_le_bytes());
    payload
}

fn record_payload_v2(
    pubkey: &[u8; 32],
    successor: &[u8; 32],
    seq: u64,
    lofts: &[String],
    pow_min: u32,
    attribution_requirement: Option<&AttributionRequirement>,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(SIG_DOMAIN_RECORD_V2.len() + 108 + lofts.len() * 32);
    payload.extend_from_slice(SIG_DOMAIN_RECORD_V2);
    payload.push(AGENT_RECORD_VERSION);
    payload.extend_from_slice(pubkey);
    payload.extend_from_slice(successor);
    payload.extend_from_slice(&seq.to_be_bytes());
    payload.extend_from_slice(&(lofts.len() as u32).to_be_bytes());
    for loft in lofts {
        payload.extend_from_slice(&(loft.len() as u32).to_be_bytes());
        payload.extend_from_slice(loft.as_bytes());
    }
    payload.extend_from_slice(&pow_min.to_be_bytes());
    match attribution_requirement {
        Some(requirement) => {
            payload.push(1);
            payload.push(requirement.version);
            payload.push(requirement.jurisdiction.into());
            payload.extend_from_slice(&requirement.authority);
        }
        None => payload.push(0),
    }
    payload
}

const fn legacy_agent_record_version() -> u8 {
    LEGACY_AGENT_RECORD_VERSION
}

fn rotation_body(
    version: u8,
    from: &[u8; 32],
    to: &[u8; 32],
    next: &[u8; 32],
    seq: u64,
    activated_at: u64,
    grace_until: u64,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(1 + 32 * 3 + 8 * 3);
    body.push(version);
    body.extend_from_slice(from);
    body.extend_from_slice(to);
    body.extend_from_slice(next);
    body.extend_from_slice(&seq.to_le_bytes());
    body.extend_from_slice(&activated_at.to_le_bytes());
    body.extend_from_slice(&grace_until.to_le_bytes());
    body
}

fn rotation_signing_payload(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(domain.len() + body.len());
    payload.extend_from_slice(domain);
    payload.extend_from_slice(body);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigeonpost_compliance_format::Jurisdiction;

    fn setup() -> (Identity, Identity, SuccessorCommitment) {
        let operating = Identity::from_seed([1; 32]);
        let successor = Identity::from_seed([2; 32]);
        let commitment = SuccessorCommitment::for_key(&successor.verifying_key());
        (operating, successor, commitment)
    }

    fn legacy_record(
        identity: &Identity,
        commitment: &SuccessorCommitment,
        seq: u64,
    ) -> AgentRecord {
        let lofts = vec!["https://legacy.example".to_owned()];
        let pow_min = 17;
        let signature = identity
            .sign(&record_payload_v1(
                &identity.verifying_key().to_bytes(),
                commitment.as_bytes(),
                seq,
                &lofts,
                pow_min,
            ))
            .to_bytes();
        AgentRecord {
            version: LEGACY_AGENT_RECORD_VERSION,
            pubkey: identity.verifying_key().to_bytes(),
            successor_hash: *commitment.as_bytes(),
            seq,
            lofts,
            pow_min,
            attribution_requirement: None,
            signature,
        }
    }

    #[test]
    fn a_record_verifies_against_its_own_address() {
        let (operating, _, commitment) = setup();
        let record = AgentRecord::new(&operating, &commitment, 0, vec!["https://a.example".into()]);
        assert!(record.verify(&operating.address()).is_ok());
    }

    #[test]
    fn a_valid_record_for_another_agent_is_rejected() {
        let (operating, _, commitment) = setup();
        let other = Identity::from_seed([9; 32]);
        let record = AgentRecord::new(&operating, &commitment, 0, vec![]);

        assert_eq!(
            record.verify(&other.address()),
            Err(Error::MalformedAddress("record is for a different address")),
            "a hostile directory must not be able to answer with someone else's valid record"
        );
    }

    #[test]
    fn tampering_with_the_loft_list_invalidates_the_record() {
        let (operating, _, commitment) = setup();
        let mut record = AgentRecord::new(
            &operating,
            &commitment,
            0,
            vec!["https://good.example".into()],
        );
        record.lofts = vec!["https://attacker.example".into()];

        assert_eq!(
            record.verify(&operating.address()),
            Err(Error::BadSignature),
            "redirecting an agent's mail must require its key"
        );
    }

    #[test]
    fn agent_record_routing_sets_are_bounded_before_signature_use() {
        let (operating, _, commitment) = setup();
        let too_many = (0..=MAX_AGENT_RECORD_LOFTS)
            .map(|index| format!("https://loft-{index}.example"))
            .collect();
        let record = AgentRecord::new(&operating, &commitment, 0, too_many);
        assert_eq!(record.verify(&operating.address()), Err(Error::TooLarge));

        let oversized = format!(
            "https://{}.example",
            "a".repeat(MAX_AGENT_RECORD_LOFT_URL_BYTES)
        );
        let record = AgentRecord::new(&operating, &commitment, 0, vec![oversized]);
        assert_eq!(record.verify(&operating.address()), Err(Error::TooLarge));
    }

    #[test]
    fn agent_record_origins_reject_ambiguous_or_unsafe_shapes() {
        for origin in [
            "wss://loft.example",
            "http://loft.example",
            "https://localhost",
            "https://localhost.",
            "https://api.localhost",
            "https://user@loft.example",
            "https://loft.example/internal",
            "https://loft.example?next=http://127.0.0.1",
            "https://loft.example#fragment",
            "https://loft.example:0",
        ] {
            let lofts = vec![origin.to_owned()];
            assert!(validate_loft_list(&lofts).is_err(), "{origin}");
        }

        assert!(validate_loft_list(&["http://127.0.0.1:7717".into()]).is_ok());
        assert!(validate_loft_list(&["http://[::1]:7717".into()]).is_ok());
        assert!(validate_loft_list(&["https://loft.example".into()]).is_ok());
    }

    #[test]
    fn loft_list_is_unambiguously_encoded() {
        let (operating, _, commitment) = setup();
        let mut a = AgentRecord::new(&operating, &commitment, 0, vec!["ab".into(), "c".into()]);
        let b = AgentRecord::new(&operating, &commitment, 0, vec!["a".into(), "bc".into()]);
        assert_ne!(a.signature, b.signature);

        a.lofts = vec!["a".into(), "bc".into()];
        assert!(a.verify(&operating.address()).is_err());
    }

    #[test]
    fn the_advertised_pow_floor_is_signed() {
        // A loft must not be able to inflate the floor to price a sender out.
        let (operating, _, commitment) = setup();
        let mut record = AgentRecord::with_pow(&operating, &commitment, 0, vec![], 18);
        assert!(record.verify(&operating.address()).is_ok());

        record.pow_min = 30;
        assert_eq!(
            record.verify(&operating.address()),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn exact_attribution_scope_is_signed() {
        let (operating, _, commitment) = setup();
        let requirement = AttributionRequirement::new(Jurisdiction::Test, [0xA5; 32]);
        let mut record = AgentRecord::with_policy(
            &operating,
            &commitment,
            4,
            vec!["https://loft.example".into()],
            18,
            Some(requirement),
        );
        assert!(record.verify(&operating.address()).is_ok());

        record.attribution_requirement.as_mut().unwrap().authority[0] ^= 1;
        assert_eq!(
            record.verify(&operating.address()),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn v1_record_migration_requires_owner_and_higher_sequence() {
        let (operating, _, commitment) = setup();
        let attacker = Identity::from_seed([9; 32]);
        let legacy = legacy_record(&operating, &commitment, 7);
        let requirement = AttributionRequirement::new(Jurisdiction::Test, [0xA5; 32]);

        assert!(legacy.verify(&operating.address()).is_ok());
        assert_eq!(
            legacy.migrate_v1(&attacker, 8, Some(requirement)),
            Err(Error::InvalidKey)
        );
        assert_eq!(
            legacy.migrate_v1(&operating, 7, Some(requirement)),
            Err(Error::StaleSequence)
        );
        let migrated = legacy.migrate_v1(&operating, 8, Some(requirement)).unwrap();
        assert_eq!(migrated.version, AGENT_RECORD_VERSION);
        assert_eq!(migrated.attribution_requirement, Some(requirement));
        assert!(migrated.verify(&operating.address()).is_ok());
    }

    #[test]
    fn rotation_to_the_committed_successor_succeeds() {
        let (operating, successor, commitment) = setup();
        let next = SuccessorCommitment::for_key(&Identity::from_seed([3; 32]).verifying_key());
        let rotation = RotationRecord::new(&operating, &successor, &next, 1, 1_000).unwrap();

        assert_eq!(
            rotation.verify(&commitment, 0, 1_000).unwrap().incoming,
            successor.verifying_key()
        );
        assert!(rotation.retired_key_is_active(1_000));
        assert!(!rotation.retired_key_is_active(rotation.grace_until));
    }

    #[test]
    fn a_compromised_key_cannot_rotate_to_an_attacker_key() {
        // The whole point of pre-commitment: the attacker holds the operating key.
        let (operating, _, commitment) = setup();
        let attacker = Identity::from_seed([66; 32]);
        let next = SuccessorCommitment::for_key(&attacker.verifying_key());
        let rotation = RotationRecord::new(&operating, &attacker, &next, 1, 1_000).unwrap();

        assert_eq!(
            rotation.verify(&commitment, 0, 1_000),
            Err(Error::SuccessorMismatch)
        );
    }

    #[test]
    fn a_compromised_outgoing_key_cannot_poison_the_incoming_commitment() {
        let (operating, successor, commitment) = setup();
        let attacker = Identity::from_seed([66; 32]);
        let attacker_next = SuccessorCommitment::for_key(&attacker.verifying_key());
        let mut rotation =
            RotationRecord::new(&operating, &successor, &attacker_next, 1, 1_000).unwrap();

        // Model K1 compromise after K2 signed the owner's transition: K1 can replace its own
        // signature, but cannot make K2 authenticate a different next commitment.
        rotation.next_successor_hash = [77; 32];
        let body = rotation_body(
            rotation.version,
            &rotation.from_pubkey,
            &rotation.to_pubkey,
            &rotation.next_successor_hash,
            rotation.seq,
            rotation.activated_at,
            rotation.grace_until,
        );
        rotation.outgoing_signature = operating
            .sign(&rotation_signing_payload(
                SIG_DOMAIN_ROTATION_OUTGOING,
                &body,
            ))
            .to_bytes();

        assert_eq!(
            rotation.verify(&commitment, 0, 1_000),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn replaying_an_old_rotation_is_rejected() {
        let (operating, successor, commitment) = setup();
        let next = SuccessorCommitment::for_key(&Identity::from_seed([3; 32]).verifying_key());
        let rotation = RotationRecord::new(&operating, &successor, &next, 5, 1_000).unwrap();

        assert!(rotation.verify(&commitment, 4, 1_000).is_ok());
        assert_eq!(
            rotation.verify(&commitment, 5, 1_000),
            Err(Error::StaleSequence),
            "equal seq is a replay"
        );
        assert_eq!(
            rotation.verify(&commitment, 9, 1_000),
            Err(Error::StaleSequence)
        );
        assert_eq!(
            rotation.verify(&commitment, 3, 1_000),
            Err(Error::StaleSequence),
            "skipping a link is not a monotonic chain"
        );
    }

    #[test]
    fn a_rotation_signed_by_the_wrong_key_is_rejected() {
        let (operating, successor, commitment) = setup();
        let impostor = Identity::from_seed([77; 32]);
        let next = SuccessorCommitment::for_key(&successor.verifying_key());
        let mut rotation = RotationRecord::new(&impostor, &successor, &next, 1, 1_000).unwrap();
        rotation.from_pubkey = operating.verifying_key().to_bytes();

        assert_eq!(
            rotation.verify(&commitment, 0, 1_000),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn future_activation_and_timing_tampering_are_rejected() {
        let (operating, successor, commitment) = setup();
        let next = SuccessorCommitment::for_key(&Identity::from_seed([3; 32]).verifying_key());
        let mut rotation = RotationRecord::new(&operating, &successor, &next, 1, 10_000).unwrap();

        assert_eq!(
            rotation.verify(&commitment, 0, 9_000),
            Err(Error::StaleTimestamp)
        );
        rotation.grace_until += 1;
        assert_eq!(
            rotation.verify(&commitment, 0, 10_000),
            Err(Error::MalformedEnvelope("invalid rotation record"))
        );
    }

    #[test]
    fn source_address_is_bound_to_the_outgoing_key() {
        let (operating, successor, _) = setup();
        let next = SuccessorCommitment::for_key(&Identity::from_seed([3; 32]).verifying_key());
        let rotation = RotationRecord::new(&operating, &successor, &next, 1, 1_000).unwrap();
        assert!(rotation.verify_source_address(&operating.address()).is_ok());
        assert!(rotation
            .verify_source_address(&Identity::from_seed([9; 32]).address())
            .is_err());
    }

    #[test]
    fn rotation_properties_hold_across_keys_sequences_and_times() {
        for seed in 1u8..=64 {
            let operating = Identity::from_seed([seed; 32]);
            let successor = Identity::from_seed([seed.wrapping_add(64); 32]);
            let next = Identity::from_seed([seed.wrapping_add(128); 32]);
            let attacker = Identity::from_seed([seed.wrapping_add(192); 32]);
            let pinned = SuccessorCommitment::for_key(&successor.verifying_key());
            let seq = u64::from(seed);
            let activated_at = 1_000_000 + u64::from(seed);
            let rotation = RotationRecord::new(
                &operating,
                &successor,
                &SuccessorCommitment::for_key(&next.verifying_key()),
                seq,
                activated_at,
            )
            .unwrap();

            let verified = rotation.verify(&pinned, seq - 1, activated_at).unwrap();
            assert_eq!(verified.incoming, successor.verifying_key());
            assert_eq!(
                verified.next_successor,
                SuccessorCommitment::for_key(&next.verifying_key())
            );
            assert!(rotation.retired_key_is_active(activated_at));
            assert!(!rotation.retired_key_is_active(rotation.grace_until));

            let hostile_pin = SuccessorCommitment::for_key(&attacker.verifying_key());
            assert_eq!(
                rotation.verify(&hostile_pin, seq - 1, activated_at),
                Err(Error::SuccessorMismatch)
            );
            assert_eq!(
                rotation.verify(&pinned, seq, activated_at),
                Err(Error::StaleSequence)
            );
        }
    }
}
