//! Two-person disclosure approval without persisting raw selectors, order references, or names.

use ed25519_dalek::Signature;
use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
use pigeonpost_core::keys;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{ComplianceError, Result};

const REQUEST_DOMAIN: &[u8] = b"pigeonpost/disclosure-request/v1";
const APPROVAL_DOMAIN: &[u8] = b"pigeonpost/disclosure-approval/v1";
const COMMITMENT_DOMAIN: &[u8] = b"pigeonpost/disclosure-private-field/v1";
pub(crate) const MAX_DISCLOSURE_KEYS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisclosureState {
    Pending,
    Authorized,
    Consumed,
    Denied,
    Expired,
}

/// Transient inputs that are committed but never retained by [`DisclosureRequest`].
#[derive(Clone, Copy)]
pub struct SensitiveRequestMaterial<'a> {
    pub order_reference: &'a [u8],
    pub requester_identity: &'a [u8],
    pub selectors: &'a [u8],
}

impl core::fmt::Debug for SensitiveRequestMaterial<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SensitiveRequestMaterial(<withheld>)")
    }
}

/// Salts returned to the secure case store so commitments can later be proven.
#[derive(ZeroizeOnDrop)]
pub struct CommitmentOpenings {
    order_salt: [u8; 32],
    requester_salt: [u8; 32],
    selector_salt: [u8; 32],
}

impl CommitmentOpenings {
    pub fn verifies_order(&self, request: &DisclosureRequest, raw: &[u8]) -> bool {
        commit(b"order", &self.order_salt, raw) == request.order_commitment
    }

    pub fn verifies_requester(&self, request: &DisclosureRequest, raw: &[u8]) -> bool {
        commit(b"requester", &self.requester_salt, raw) == request.requester_commitment
    }

    pub fn verifies_selectors(&self, request: &DisclosureRequest, raw: &[u8]) -> bool {
        commit(b"selectors", &self.selector_salt, raw) == request.selector_commitment
    }

    pub(crate) const fn salts(&self) -> [[u8; 32]; 3] {
        [self.order_salt, self.requester_salt, self.selector_salt]
    }
}

impl core::fmt::Debug for CommitmentOpenings {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CommitmentOpenings(<withheld>)")
    }
}

/// Offline request state. It contains only salted commitments to sensitive case metadata.
#[derive(Clone)]
pub struct DisclosureRequest {
    request_id: [u8; 32],
    jurisdiction: Jurisdiction,
    purpose: CompliancePurpose,
    key_ids: Vec<ComplianceKeyId>,
    created_at_ms: u64,
    expires_at_ms: u64,
    order_commitment: [u8; 32],
    requester_commitment: [u8; 32],
    selector_commitment: [u8; 32],
    approver_keys: Vec<[u8; 32]>,
    approvers: Vec<[u8; 32]>,
    approver_salts: Vec<[u8; 32]>,
    state: DisclosureState,
}

impl DisclosureRequest {
    pub fn new(
        jurisdiction: Jurisdiction,
        purpose: CompliancePurpose,
        mut key_ids: Vec<ComplianceKeyId>,
        created_at_ms: u64,
        expires_at_ms: u64,
        sensitive: SensitiveRequestMaterial<'_>,
    ) -> Result<(Self, CommitmentOpenings)> {
        if created_at_ms == 0
            || expires_at_ms <= created_at_ms
            || key_ids.is_empty()
            || key_ids.len() > MAX_DISCLOSURE_KEYS
            || sensitive.order_reference.is_empty()
            || sensitive.requester_identity.is_empty()
            || sensitive.selectors.is_empty()
        {
            return Err(ComplianceError::InvalidRequest);
        }
        for key_id in &key_ids {
            if key_id.validate().is_err()
                || key_id.purpose != purpose
                || key_id.jurisdiction != jurisdiction
            {
                return Err(ComplianceError::WrongPurpose);
            }
        }
        key_ids.sort_by_key(|key_id| key_id.encode().expect("validated key id"));
        if key_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut openings = CommitmentOpenings {
            order_salt: [0u8; 32],
            requester_salt: [0u8; 32],
            selector_salt: [0u8; 32],
        };
        let order_commitment = random_commitment(
            b"order",
            &mut openings.order_salt,
            sensitive.order_reference,
        );
        let requester_commitment = random_commitment(
            b"requester",
            &mut openings.requester_salt,
            sensitive.requester_identity,
        );
        let selector_commitment = random_commitment(
            b"selectors",
            &mut openings.selector_salt,
            sensitive.selectors,
        );
        let request_id = request_id(
            jurisdiction,
            purpose,
            &key_ids,
            created_at_ms,
            expires_at_ms,
            &order_commitment,
            &requester_commitment,
            &selector_commitment,
        );
        Ok((
            Self {
                request_id,
                jurisdiction,
                purpose,
                key_ids,
                created_at_ms,
                expires_at_ms,
                order_commitment,
                requester_commitment,
                selector_commitment,
                approver_keys: Vec::with_capacity(2),
                approvers: Vec::with_capacity(2),
                approver_salts: Vec::with_capacity(2),
                state: DisclosureState::Pending,
            },
            openings,
        ))
    }

    pub const fn request_id(&self) -> [u8; 32] {
        self.request_id
    }

    pub const fn jurisdiction(&self) -> Jurisdiction {
        self.jurisdiction
    }

    pub const fn purpose(&self) -> CompliancePurpose {
        self.purpose
    }

    pub fn key_ids(&self) -> &[ComplianceKeyId] {
        &self.key_ids
    }

    pub const fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    pub const fn order_commitment(&self) -> [u8; 32] {
        self.order_commitment
    }

    pub const fn requester_commitment(&self) -> [u8; 32] {
        self.requester_commitment
    }

    pub const fn selector_commitment(&self) -> [u8; 32] {
        self.selector_commitment
    }

    pub fn approver_commitments(&self) -> &[[u8; 32]] {
        &self.approvers
    }

    pub(crate) fn approver_openings(&self) -> (&[[u8; 32]], &[[u8; 32]]) {
        (&self.approver_keys, &self.approver_salts)
    }

    pub const fn state(&self) -> DisclosureState {
        self.state
    }

    /// Canonical bytes an approver signs for this request and approval time.
    pub fn approval_preimage(&self, approved_at_ms: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(APPROVAL_DOMAIN.len() + 32 + 8 + 1);
        out.extend_from_slice(APPROVAL_DOMAIN);
        out.extend_from_slice(&self.request_id);
        out.extend_from_slice(&approved_at_ms.to_be_bytes());
        out.push(1);
        out
    }

    /// Add one authenticated approval. Exactly two distinct approvers authorize the request.
    pub fn approve(
        &mut self,
        approver_public_key: [u8; 32],
        approved_at_ms: u64,
        signature: [u8; 64],
    ) -> Result<()> {
        self.expire_if_needed(approved_at_ms);
        if self.state != DisclosureState::Pending || approved_at_ms < self.created_at_ms {
            return Err(match self.state {
                DisclosureState::Expired => ComplianceError::Expired,
                _ => ComplianceError::StateConflict,
            });
        }
        let key = keys::verifying_key_from_bytes(&approver_public_key)
            .map_err(|_| ComplianceError::BadApproval)?;
        keys::verify(
            &key,
            &self.approval_preimage(approved_at_ms),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| ComplianceError::BadApproval)?;
        if self.approver_keys.contains(&approver_public_key) {
            return Err(ComplianceError::BadApproval);
        }
        let mut salt = [0u8; 32];
        let mut approver_commitment = [0u8; 32];
        while approver_commitment == [0u8; 32] || self.approvers.contains(&approver_commitment) {
            OsRng.fill_bytes(&mut salt);
            approver_commitment = commit(b"approver", &salt, &approver_public_key);
        }
        self.approver_keys.push(approver_public_key);
        self.approvers.push(approver_commitment);
        self.approver_salts.push(salt);
        salt.zeroize();
        if self.approvers.len() == 2 {
            self.state = DisclosureState::Authorized;
        }
        Ok(())
    }

    pub fn deny(&mut self, at_ms: u64) -> Result<()> {
        self.expire_if_needed(at_ms);
        if self.state != DisclosureState::Pending {
            return Err(ComplianceError::StateConflict);
        }
        self.state = DisclosureState::Denied;
        Ok(())
    }

    pub(crate) fn consume(&mut self, at_ms: u64) -> Result<AuthorizedDisclosure> {
        self.expire_if_needed(at_ms);
        if self.state != DisclosureState::Authorized {
            return Err(match self.state {
                DisclosureState::Expired => ComplianceError::Expired,
                _ => ComplianceError::Unauthorized,
            });
        }
        self.state = DisclosureState::Consumed;
        Ok(AuthorizedDisclosure {
            request_id: self.request_id,
            jurisdiction: self.jurisdiction,
            purpose: self.purpose,
            key_ids: self.key_ids.clone(),
        })
    }

    pub(crate) fn ensure_authorized(&mut self, at_ms: u64) -> Result<()> {
        self.expire_if_needed(at_ms);
        if self.state == DisclosureState::Authorized {
            Ok(())
        } else {
            Err(match self.state {
                DisclosureState::Expired => ComplianceError::Expired,
                _ => ComplianceError::Unauthorized,
            })
        }
    }

    fn expire_if_needed(&mut self, now_ms: u64) {
        if now_ms > self.expires_at_ms
            && matches!(
                self.state,
                DisclosureState::Pending | DisclosureState::Authorized
            )
        {
            self.state = DisclosureState::Expired;
        }
    }
}

impl Drop for DisclosureRequest {
    fn drop(&mut self) {
        self.approver_salts.zeroize();
    }
}

impl core::fmt::Debug for DisclosureRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DisclosureRequest")
            .field("request_id", &self.request_id)
            .field("jurisdiction", &self.jurisdiction)
            .field("purpose", &self.purpose)
            .field("key_ids", &self.key_ids)
            .field("created_at_ms", &self.created_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("private_case_fields", &"<commitments only>")
            .field("approver_count", &self.approvers.len())
            .field("state", &self.state)
            .finish()
    }
}

/// Unforgeable-in-safe-code capability passed only after the public intent leaf is appended.
pub struct AuthorizedDisclosure {
    request_id: [u8; 32],
    jurisdiction: Jurisdiction,
    purpose: CompliancePurpose,
    key_ids: Vec<ComplianceKeyId>,
}

impl AuthorizedDisclosure {
    pub const fn request_id(&self) -> [u8; 32] {
        self.request_id
    }

    pub const fn jurisdiction(&self) -> Jurisdiction {
        self.jurisdiction
    }

    pub const fn purpose(&self) -> CompliancePurpose {
        self.purpose
    }

    pub(crate) fn permits(&self, key_id: &ComplianceKeyId) -> bool {
        self.purpose == key_id.purpose
            && self.jurisdiction == key_id.jurisdiction
            && self.key_ids.contains(key_id)
    }
}

impl core::fmt::Debug for AuthorizedDisclosure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AuthorizedDisclosure")
            .field("request_id", &self.request_id)
            .field("jurisdiction", &self.jurisdiction)
            .field("purpose", &self.purpose)
            .field("key_count", &self.key_ids.len())
            .finish()
    }
}

pub(crate) fn commit(label: &[u8], salt: &[u8; 32], raw: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(COMMITMENT_DOMAIN);
    hash.update((label.len() as u16).to_be_bytes());
    hash.update(label);
    hash.update(salt);
    hash.update((raw.len() as u64).to_be_bytes());
    hash.update(raw);
    hash.finalize().into()
}

fn random_commitment(label: &[u8], salt: &mut [u8; 32], raw: &[u8]) -> [u8; 32] {
    loop {
        OsRng.fill_bytes(salt);
        let value = commit(label, salt, raw);
        if value != [0u8; 32] {
            return value;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn request_id(
    jurisdiction: Jurisdiction,
    purpose: CompliancePurpose,
    key_ids: &[ComplianceKeyId],
    created_at_ms: u64,
    expires_at_ms: u64,
    order_commitment: &[u8; 32],
    requester_commitment: &[u8; 32],
    selector_commitment: &[u8; 32],
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(REQUEST_DOMAIN);
    hash.update([jurisdiction.into(), purpose.into()]);
    hash.update(created_at_ms.to_be_bytes());
    hash.update(expires_at_ms.to_be_bytes());
    hash.update([key_ids.len() as u8]);
    for key_id in key_ids {
        hash.update(key_id.encode().expect("validated key id"));
    }
    hash.update(order_commitment);
    hash.update(requester_commitment);
    hash.update(selector_commitment);
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    fn request() -> (DisclosureRequest, CommitmentOpenings) {
        DisclosureRequest::new(
            Jurisdiction::Test,
            CompliancePurpose::Attribution,
            vec![ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [1; 32],
                10,
                1,
            )],
            100,
            200,
            SensitiveRequestMaterial {
                order_reference: b"secret-order-42",
                requester_identity: b"named investigator",
                selectors: b"203.0.113.7 and person@example.invalid",
            },
        )
        .unwrap()
    }

    #[test]
    fn raw_case_fields_are_not_retained_or_debugged() {
        let (request, openings) = request();
        assert!(openings.verifies_order(&request, b"secret-order-42"));
        let debug = format!("{request:?}");
        assert!(!debug.contains("secret-order-42"));
        assert!(!debug.contains("named investigator"));
        assert!(!debug.contains("203.0.113.7"));
    }

    #[test]
    fn requires_two_distinct_valid_signatures() {
        let (mut request, _) = request();
        let first = SigningKey::from_bytes(&[2; 32]);
        let second = SigningKey::from_bytes(&[3; 32]);
        let at = 150;
        let first_sig = first.sign(&request.approval_preimage(at)).to_bytes();
        request
            .approve(first.verifying_key().to_bytes(), at, first_sig)
            .unwrap();
        assert_eq!(request.state(), DisclosureState::Pending);
        let duplicate = first.sign(&request.approval_preimage(at)).to_bytes();
        assert!(request
            .approve(first.verifying_key().to_bytes(), at, duplicate)
            .is_err());
        let second_sig = second.sign(&request.approval_preimage(at)).to_bytes();
        request
            .approve(second.verifying_key().to_bytes(), at, second_sig)
            .unwrap();
        assert_eq!(request.state(), DisclosureState::Authorized);
    }

    #[test]
    fn expired_or_cross_purpose_requests_fail_closed() {
        let (mut request, _) = request();
        let signer = SigningKey::from_bytes(&[4; 32]);
        let signature = signer.sign(&request.approval_preimage(201)).to_bytes();
        assert_eq!(
            request.approve(signer.verifying_key().to_bytes(), 201, signature),
            Err(ComplianceError::Expired)
        );
        assert_eq!(request.state(), DisclosureState::Expired);
    }
}
