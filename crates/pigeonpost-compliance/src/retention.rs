//! Retention, legal-hold, and complete-copy destruction accounting.

use ed25519_dalek::Signature;
use pigeonpost_compliance_format::{
    compliance_epoch_end_ms, ComplianceKeyId, Jurisdiction, COMPLIANCE_KEY_ID_LEN,
};
use pigeonpost_core::keys;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::error::{ComplianceError, Result};

const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
const COPY_COMMITMENT_DOMAIN: &[u8] = b"pigeonpost/key-copy-location/v1";
const ABSENCE_COMMITMENT_DOMAIN: &[u8] = b"pigeonpost/key-copy-absence/v1";
const HOLD_ID_DOMAIN: &[u8] = b"pigeonpost/legal-hold-id/v2";
const HOLD_RECEIPT_HASH_DOMAIN: &[u8] = b"pigeonpost/legal-hold-receipt-hash/v1";
const HOLD_APPROVER_KEY_ID_DOMAIN: &[u8] = b"pigeonpost/legal-hold-approver-key/v1";
const DISCLOSURE_REQUEST_DOMAIN: &[u8] = b"pigeonpost/disclosure-request/v1";
const DISCLOSURE_APPROVAL_DOMAIN: &[u8] = b"pigeonpost/disclosure-approval/v1";
const DISCLOSURE_COMMITMENT_DOMAIN: &[u8] = b"pigeonpost/disclosure-private-field/v1";
const LEGAL_PROCESS_REQUESTER: &[u8] = b"legal-process-intake";
const HOLD_RECEIPT_VERSION: u8 = 1;
const MAX_HOLD_TERM_MS: u64 = 90 * DAY_MS;
const INVENTORY_MAGIC: &[u8; 8] = b"PPINV\0\0\0";
const INVENTORY_VERSION: u8 = 3;
const LEGACY_INVENTORY_VERSION: u8 = 2;
const RETENTION_POLICY_VERSION: u8 = 1;
const US_RETENTION_DAYS: u16 = 30;
const EU_RETENTION_DAYS: u16 = 0;
const TEST_RETENTION_DAYS: u16 = 1;
pub const TR_RETENTION_DAYS_MIN: u16 = 365;
pub const TR_RETENTION_DAYS_MAX: u16 = 730;
const MAX_INVENTORY_COPIES: usize = 64;
const MAX_INVENTORY_HOLDS: usize = 64;
const POLICY_ENCODED_LEN: usize = 1 + 2 + 2 + 2 + 2 + 32;
const COPY_ENCODED_LEN: usize = 32 + 1 + 1 + 1 + 32;
const HOLD_APPROVAL_ENCODED_LEN: usize = 32 + 32 + 8 + 64;
const HOLD_AUTHORIZATION_ENCODED_LEN: usize =
    32 + 8 + 8 + 32 + 32 + 32 + 3 * 32 + 2 * HOLD_APPROVAL_ENCODED_LEN;
const HOLD_RECEIPT_ENCODED_LEN: usize = 1
    + 1
    + 32
    + COMPLIANCE_KEY_ID_LEN
    + 8
    + 8
    + 8
    + 1
    + 32
    + 1
    + 32
    + HOLD_AUTHORIZATION_ENCODED_LEN;

/// Storage classes that must be affirmatively inventoried before shredding starts. A deployment
/// that does not use a class records a verified-absent entry instead of silently omitting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum KeyCopyKind {
    LiveMetadata = 1,
    SqliteWal = 2,
    Sidecar = 3,
    Snapshot = 4,
    Backup = 5,
    KmsVersion = 6,
    ShamirShare = 7,
}

const REQUIRED_COPY_KINDS: [KeyCopyKind; 7] = [
    KeyCopyKind::LiveMetadata,
    KeyCopyKind::SqliteWal,
    KeyCopyKind::Sidecar,
    KeyCopyKind::Snapshot,
    KeyCopyKind::Backup,
    KeyCopyKind::KmsVersion,
    KeyCopyKind::ShamirShare,
];

/// Versioned counsel-approved retention decision embedded in every inventory.
///
/// The product choices for the US, EU, and test jurisdiction are explicit and immutable within
/// policy version 1. Counsel selects the Türkiye duration within the documented statutory band and
/// supplies a non-secret commitment to the approval record. A new Türkiye duration therefore
/// requires a new private policy/configuration value, not a code release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    version: u8,
    us_days: u16,
    eu_days: u16,
    tr_days: u16,
    test_days: u16,
    counsel_approval_commitment: [u8; 32],
}

impl RetentionPolicy {
    pub fn new(tr_days: u16, counsel_approval_commitment: [u8; 32]) -> Result<Self> {
        let policy = Self {
            version: RETENTION_POLICY_VERSION,
            us_days: US_RETENTION_DAYS,
            eu_days: EU_RETENTION_DAYS,
            tr_days,
            test_days: TEST_RETENTION_DAYS,
            counsel_approval_commitment,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub const fn version(&self) -> u8 {
        self.version
    }

    pub const fn tr_days(&self) -> u16 {
        self.tr_days
    }

    pub const fn counsel_approval_commitment(&self) -> [u8; 32] {
        self.counsel_approval_commitment
    }

    /// Compute expiry from the canonical exclusive end of the key's purpose-specific epoch.
    /// Starting from creation/epoch-open time would shorten retention for late records and could
    /// make a zero-day attribution key shreddable while its calendar month is still active.
    pub fn retention_until(&self, key_id: &ComplianceKeyId) -> Result<u64> {
        self.validate()?;
        let epoch_end_ms =
            compliance_epoch_end_ms(key_id).map_err(|_| ComplianceError::IncompleteInventory)?;
        let days = match key_id.jurisdiction {
            Jurisdiction::Us => self.us_days,
            Jurisdiction::Eu => self.eu_days,
            Jurisdiction::Tr => self.tr_days,
            Jurisdiction::Test => self.test_days,
        };
        epoch_end_ms
            .checked_add(u64::from(days) * DAY_MS)
            .ok_or(ComplianceError::InvalidRequest)
    }

    fn validate(&self) -> Result<()> {
        if self.version != RETENTION_POLICY_VERSION
            || self.us_days != US_RETENTION_DAYS
            || self.eu_days != EU_RETENTION_DAYS
            || !(TR_RETENTION_DAYS_MIN..=TR_RETENTION_DAYS_MAX).contains(&self.tr_days)
            || self.test_days != TEST_RETENTION_DAYS
            || self.counsel_approval_commitment == [0u8; 32]
        {
            return Err(ComplianceError::IncompleteInventory);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyState {
    Present,
    DestructionRequested,
    Destroyed,
    VerifiedAbsent,
}

/// One committed storage location. Raw paths, KMS resource names, and share-holder identities are
/// consumed transiently and never stored.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyCopy {
    copy_id: [u8; 32],
    kind: KeyCopyKind,
    state: CopyState,
    destruction_evidence: Option<[u8; 32]>,
}

impl KeyCopy {
    pub fn present(kind: KeyCopyKind, raw_locator: &[u8]) -> Result<Self> {
        let nonce = random_nonzero_nonce();
        Self::present_with_nonce(kind, nonce, raw_locator)
    }

    /// Commit a private locator using a caller-provided unique nonce. This is used by the offline
    /// inventory declaration ceremony so retries reproduce the same copy identifier without ever
    /// storing the locator itself.
    pub fn present_with_nonce(
        kind: KeyCopyKind,
        nonce: [u8; 32],
        raw_locator: &[u8],
    ) -> Result<Self> {
        let copy_id = copy_commitment(COPY_COMMITMENT_DOMAIN, kind, nonce, raw_locator)?;
        Ok(Self {
            copy_id,
            kind,
            state: CopyState::Present,
            destruction_evidence: None,
        })
    }

    pub fn verified_absent(kind: KeyCopyKind, absence_evidence: &[u8]) -> Result<Self> {
        let nonce = random_nonzero_nonce();
        Self::verified_absent_with_nonce(kind, nonce, absence_evidence)
    }

    /// Commit private verification material for a storage class that does not exist.
    pub fn verified_absent_with_nonce(
        kind: KeyCopyKind,
        nonce: [u8; 32],
        absence_evidence: &[u8],
    ) -> Result<Self> {
        let copy_id = copy_commitment(COPY_COMMITMENT_DOMAIN, kind, nonce, absence_evidence)?;
        let evidence = copy_commitment(ABSENCE_COMMITMENT_DOMAIN, kind, nonce, absence_evidence)?;
        Ok(Self {
            copy_id,
            kind,
            state: CopyState::VerifiedAbsent,
            destruction_evidence: Some(evidence),
        })
    }

    pub const fn copy_id(&self) -> [u8; 32] {
        self.copy_id
    }

    pub const fn kind(&self) -> KeyCopyKind {
        self.kind
    }

    pub const fn state(&self) -> CopyState {
        self.state
    }
}

impl core::fmt::Debug for KeyCopy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyCopy")
            .field("copy_id", &self.copy_id)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .field(
                "destruction_evidence",
                &self.destruction_evidence.map(|_| "<commitment>"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryState {
    Retained,
    Shredding,
    Shredded,
}

/// Integrity of the ciphertext bundle named by an authenticated terminal trace manifest at shred
/// time. A degraded bundle is still destroyed by key, but the fact can never be rewritten as clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceIntegrityStatus {
    Verified,
    Degraded,
}

/// Durable evidence captured before a trace epoch enters `Shredding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceIntegrityEvidence {
    status: TraceIntegrityStatus,
    terminal_manifest_commitment: [u8; 32],
}

impl TraceIntegrityEvidence {
    pub(crate) fn new(
        status: TraceIntegrityStatus,
        terminal_manifest_commitment: [u8; 32],
    ) -> Result<Self> {
        if terminal_manifest_commitment == [0u8; 32] {
            return Err(ComplianceError::IncompleteInventory);
        }
        Ok(Self {
            status,
            terminal_manifest_commitment,
        })
    }

    pub const fn status(&self) -> TraceIntegrityStatus {
        self.status
    }

    pub const fn terminal_manifest_commitment(&self) -> [u8; 32] {
        self.terminal_manifest_commitment
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum LegalHoldReceiptAction {
    Place = 1,
    Renew = 2,
    Release = 3,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegalHoldApproval {
    roster_key_id: [u8; 32],
    public_key: [u8; 32],
    approved_at_ms: u64,
    signature: [u8; 64],
}

impl LegalHoldApproval {
    pub(crate) fn new(
        public_key: [u8; 32],
        approved_at_ms: u64,
        signature: [u8; 64],
    ) -> Result<Self> {
        let approval = Self {
            roster_key_id: hold_approver_key_id(&public_key),
            public_key,
            approved_at_ms,
            signature,
        };
        if approval.roster_key_id == [0u8; 32]
            || approval.public_key == [0u8; 32]
            || approval.approved_at_ms == 0
        {
            return Err(ComplianceError::BadApproval);
        }
        Ok(approval)
    }
}

impl core::fmt::Debug for LegalHoldApproval {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LegalHoldApproval")
            .field("roster_key_id", &self.roster_key_id)
            .field("public_key", &self.public_key)
            .field("approved_at_ms", &self.approved_at_ms)
            .field("signature", &"<signature>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LegalHoldAuthorization {
    request_id: [u8; 32],
    created_at_ms: u64,
    expires_at_ms: u64,
    order_commitment: [u8; 32],
    requester_commitment: [u8; 32],
    selector_commitment: [u8; 32],
    commitment_salts: [[u8; 32]; 3],
    approvals: [LegalHoldApproval; 2],
}

impl LegalHoldAuthorization {
    pub(crate) fn from_request(
        request: &crate::approval::DisclosureRequest,
        openings: &crate::approval::CommitmentOpenings,
        mut approvals: [LegalHoldApproval; 2],
    ) -> Result<Self> {
        if request.state() != crate::approval::DisclosureState::Authorized
            || request.key_ids().len() != 1
        {
            return Err(ComplianceError::Unauthorized);
        }
        approvals.sort_unstable_by_key(|approval| approval.roster_key_id);
        let (approved_keys, _) = request.approver_openings();
        if approved_keys.len() != 2
            || approvals[0].public_key == approvals[1].public_key
            || approvals[0].roster_key_id == approvals[1].roster_key_id
            || approvals
                .iter()
                .any(|approval| !approved_keys.contains(&approval.public_key))
        {
            return Err(ComplianceError::BadApproval);
        }
        Ok(Self {
            request_id: request.request_id(),
            created_at_ms: request.created_at_ms(),
            expires_at_ms: request.expires_at_ms(),
            order_commitment: request.order_commitment(),
            requester_commitment: request.requester_commitment(),
            selector_commitment: request.selector_commitment(),
            commitment_salts: openings.salts(),
            approvals,
        })
    }

    fn validate(&self, key_id: &ComplianceKeyId, selector_material: &[u8]) -> Result<()> {
        if self.created_at_ms == 0
            || self.expires_at_ms <= self.created_at_ms
            || self.order_commitment == [0u8; 32]
            || self.requester_commitment == [0u8; 32]
            || self.selector_commitment == [0u8; 32]
            || self.approvals[0].public_key == self.approvals[1].public_key
            || self.approvals[0].roster_key_id >= self.approvals[1].roster_key_id
            || disclosure_commitment(
                b"requester",
                &self.commitment_salts[1],
                LEGAL_PROCESS_REQUESTER,
            ) != self.requester_commitment
            || disclosure_commitment(b"selectors", &self.commitment_salts[2], selector_material)
                != self.selector_commitment
            || disclosure_request_id(
                key_id,
                self.created_at_ms,
                self.expires_at_ms,
                &self.order_commitment,
                &self.requester_commitment,
                &self.selector_commitment,
            ) != self.request_id
        {
            return Err(ComplianceError::BadApproval);
        }
        for approval in &self.approvals {
            if approval.roster_key_id != hold_approver_key_id(&approval.public_key)
                || approval.approved_at_ms < self.created_at_ms
                || approval.approved_at_ms > self.expires_at_ms
            {
                return Err(ComplianceError::BadApproval);
            }
            let verifying_key = keys::verifying_key_from_bytes(&approval.public_key)
                .map_err(|_| ComplianceError::BadApproval)?;
            keys::verify(
                &verifying_key,
                &disclosure_approval_preimage(&self.request_id, approval.approved_at_ms),
                &Signature::from_bytes(&approval.signature),
            )
            .map_err(|_| ComplianceError::BadApproval)?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for LegalHoldAuthorization {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LegalHoldAuthorization")
            .field("request_id", &self.request_id)
            .field("created_at_ms", &self.created_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("order_commitment", &self.order_commitment)
            .field("approvals", &self.approvals)
            .field("private_commitment_openings", &"<withheld>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LegalHoldReceipt {
    action: LegalHoldReceiptAction,
    hold_id: [u8; 32],
    key_id: ComplianceKeyId,
    starts_at_ms: u64,
    expires_at_ms: u64,
    acted_at_ms: u64,
    predecessor_hold_id: Option<[u8; 32]>,
    predecessor_receipt_hash: Option<[u8; 32]>,
    authorization: LegalHoldAuthorization,
}

impl LegalHoldReceipt {
    pub const fn hold_id(&self) -> [u8; 32] {
        self.hold_id
    }

    pub const fn order_commitment(&self) -> [u8; 32] {
        self.authorization.order_commitment
    }

    pub const fn key_id(&self) -> ComplianceKeyId {
        self.key_id
    }

    pub const fn request_id(&self) -> [u8; 32] {
        self.authorization.request_id
    }

    pub fn approval_roster(&self) -> [([u8; 32], [u8; 32]); 2] {
        self.authorization
            .approvals
            .map(|approval| (approval.roster_key_id, approval.public_key))
    }

    pub fn approval_timestamps_ms(&self) -> [u64; 2] {
        self.authorization
            .approvals
            .map(|approval| approval.approved_at_ms)
    }

    pub const fn acted_at_ms(&self) -> u64 {
        self.acted_at_ms
    }

    pub const fn predecessor_hold_id(&self) -> Option<[u8; 32]> {
        self.predecessor_hold_id
    }

    pub const fn predecessor_receipt_hash(&self) -> Option<[u8; 32]> {
        self.predecessor_receipt_hash
    }

    pub fn receipt_hash(&self) -> Result<[u8; 32]> {
        self.validate()?;
        let mut encoded = Vec::with_capacity(HOLD_RECEIPT_ENCODED_LEN);
        self.encode_into(&mut encoded)?;
        let mut hash = Sha256::new();
        hash.update(HOLD_RECEIPT_HASH_DOMAIN);
        hash.update(encoded);
        Ok(hash.finalize().into())
    }

    fn validate(&self) -> Result<()> {
        if self.key_id.validate().is_err()
            || self.hold_id == [0u8; 32]
            || self.acted_at_ms != self.authorization.created_at_ms
            || self.starts_at_ms == 0
            || self.expires_at_ms <= self.starts_at_ms
            || self.expires_at_ms - self.starts_at_ms > MAX_HOLD_TERM_MS
        {
            return Err(ComplianceError::IncompleteInventory);
        }
        let selector = hold_selector_material(
            self.action,
            self.hold_id,
            self.expires_at_ms,
            self.predecessor_hold_id,
        )?;
        self.authorization.validate(&self.key_id, &selector)?;
        match self.action {
            LegalHoldReceiptAction::Place => {
                if self.predecessor_hold_id.is_some()
                    || self.predecessor_receipt_hash.is_some()
                    || self.starts_at_ms != self.acted_at_ms
                    || self.hold_id
                        != canonical_hold_id(
                            &self.key_id,
                            self.starts_at_ms,
                            self.expires_at_ms,
                            None,
                            &self.authorization,
                        )?
                {
                    return Err(ComplianceError::IncompleteInventory);
                }
            }
            LegalHoldReceiptAction::Renew => {
                let Some(predecessor) = self.predecessor_hold_id else {
                    return Err(ComplianceError::IncompleteInventory);
                };
                if self.predecessor_receipt_hash.is_none()
                    || self.starts_at_ms != self.acted_at_ms
                    || predecessor == self.hold_id
                    || self.hold_id
                        != canonical_hold_id(
                            &self.key_id,
                            self.starts_at_ms,
                            self.expires_at_ms,
                            Some(predecessor),
                            &self.authorization,
                        )?
                {
                    return Err(ComplianceError::IncompleteInventory);
                }
            }
            LegalHoldReceiptAction::Release => {
                if self.predecessor_hold_id != Some(self.hold_id)
                    || self.predecessor_receipt_hash.is_none()
                {
                    return Err(ComplianceError::IncompleteInventory);
                }
            }
        }
        Ok(())
    }

    fn encode_into(&self, out: &mut Vec<u8>) -> Result<()> {
        out.push(HOLD_RECEIPT_VERSION);
        out.push(self.action as u8);
        out.extend_from_slice(&self.hold_id);
        out.extend_from_slice(
            &self
                .key_id
                .encode()
                .map_err(|_| ComplianceError::IncompleteInventory)?,
        );
        out.extend_from_slice(&self.starts_at_ms.to_be_bytes());
        out.extend_from_slice(&self.expires_at_ms.to_be_bytes());
        out.extend_from_slice(&self.acted_at_ms.to_be_bytes());
        push_optional32(out, self.predecessor_hold_id);
        push_optional32(out, self.predecessor_receipt_hash);
        out.extend_from_slice(&self.authorization.request_id);
        out.extend_from_slice(&self.authorization.created_at_ms.to_be_bytes());
        out.extend_from_slice(&self.authorization.expires_at_ms.to_be_bytes());
        out.extend_from_slice(&self.authorization.order_commitment);
        out.extend_from_slice(&self.authorization.requester_commitment);
        out.extend_from_slice(&self.authorization.selector_commitment);
        for salt in &self.authorization.commitment_salts {
            out.extend_from_slice(salt);
        }
        for approval in &self.authorization.approvals {
            out.extend_from_slice(&approval.roster_key_id);
            out.extend_from_slice(&approval.public_key);
            out.extend_from_slice(&approval.approved_at_ms.to_be_bytes());
            out.extend_from_slice(&approval.signature);
        }
        Ok(())
    }

    fn decode(reader: &mut InventoryReader<'_>) -> Result<Self> {
        if reader.u8()? != HOLD_RECEIPT_VERSION {
            return Err(ComplianceError::IncompleteInventory);
        }
        let action = match reader.u8()? {
            1 => LegalHoldReceiptAction::Place,
            2 => LegalHoldReceiptAction::Renew,
            3 => LegalHoldReceiptAction::Release,
            _ => return Err(ComplianceError::IncompleteInventory),
        };
        let hold_id = reader.array32()?;
        let key_id = ComplianceKeyId::decode(reader.take(COMPLIANCE_KEY_ID_LEN)?)
            .map_err(|_| ComplianceError::IncompleteInventory)?;
        let starts_at_ms = reader.u64()?;
        let expires_at_ms = reader.u64()?;
        let acted_at_ms = reader.u64()?;
        let predecessor_hold_id = reader.optional32()?;
        let predecessor_receipt_hash = reader.optional32()?;
        let request_id = reader.array32()?;
        let created_at_ms = reader.u64()?;
        let request_expires_at_ms = reader.u64()?;
        let order_commitment = reader.array32()?;
        let requester_commitment = reader.array32()?;
        let selector_commitment = reader.array32()?;
        let commitment_salts = [reader.array32()?, reader.array32()?, reader.array32()?];
        let mut approvals = Vec::with_capacity(2);
        for _ in 0..2 {
            approvals.push(LegalHoldApproval {
                roster_key_id: reader.array32()?,
                public_key: reader.array32()?,
                approved_at_ms: reader.u64()?,
                signature: reader.array64()?,
            });
        }
        let receipt = Self {
            action,
            hold_id,
            key_id,
            starts_at_ms,
            expires_at_ms,
            acted_at_ms,
            predecessor_hold_id,
            predecessor_receipt_hash,
            authorization: LegalHoldAuthorization {
                request_id,
                created_at_ms,
                expires_at_ms: request_expires_at_ms,
                order_commitment,
                requester_commitment,
                selector_commitment,
                commitment_salts,
                approvals: approvals.try_into().expect("exact approval count"),
            },
        };
        receipt.validate()?;
        Ok(receipt)
    }
}

impl core::fmt::Debug for LegalHoldReceipt {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LegalHoldReceipt")
            .field("action", &self.action)
            .field("hold_id", &self.hold_id)
            .field("key_id", &self.key_id)
            .field("starts_at_ms", &self.starts_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("acted_at_ms", &self.acted_at_ms)
            .field("predecessor_hold_id", &self.predecessor_hold_id)
            .field("authorization", &self.authorization)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LegalHold {
    term_receipt: LegalHoldReceipt,
    release_receipt: Option<LegalHoldReceipt>,
}

impl LegalHold {
    pub const fn hold_id(&self) -> [u8; 32] {
        self.term_receipt.hold_id
    }

    pub fn active_at(&self, at_ms: u64) -> bool {
        self.starts_at_ms() <= at_ms
            && at_ms <= self.expires_at_ms()
            && self
                .released_at_ms()
                .is_none_or(|released| at_ms < released)
    }

    pub const fn starts_at_ms(&self) -> u64 {
        self.term_receipt.starts_at_ms
    }

    pub const fn expires_at_ms(&self) -> u64 {
        self.term_receipt.expires_at_ms
    }

    pub fn released_at_ms(&self) -> Option<u64> {
        self.release_receipt
            .as_ref()
            .map(LegalHoldReceipt::acted_at_ms)
    }

    /// The immediately preceding hold term when this record is a renewal.
    pub const fn renews(&self) -> Option<[u8; 32]> {
        self.term_receipt.predecessor_hold_id
    }

    pub const fn term_receipt(&self) -> &LegalHoldReceipt {
        &self.term_receipt
    }

    pub fn release_receipt(&self) -> Option<&LegalHoldReceipt> {
        self.release_receipt.as_ref()
    }
}

impl core::fmt::Debug for LegalHold {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LegalHold")
            .field("term_receipt", &self.term_receipt)
            .field("release_receipt", &self.release_receipt)
            .finish()
    }
}

/// Serialized-in-memory state machine for all known copies of one epoch key.
#[derive(Debug, Clone)]
pub struct DestructionInventory {
    key_id: ComplianceKeyId,
    retention_policy: RetentionPolicy,
    created_at_ms: u64,
    retention_until_ms: u64,
    state: InventoryState,
    trace_integrity: Option<TraceIntegrityEvidence>,
    copies: Vec<KeyCopy>,
    holds: Vec<LegalHold>,
}

impl DestructionInventory {
    pub fn new(
        key_id: ComplianceKeyId,
        created_at_ms: u64,
        retention_policy: RetentionPolicy,
        mut copies: Vec<KeyCopy>,
    ) -> Result<Self> {
        canonicalize_copies(&mut copies);
        let retention_until_ms = retention_policy.retention_until(&key_id)?;
        let inventory = Self {
            key_id,
            retention_policy,
            created_at_ms,
            retention_until_ms,
            state: InventoryState::Retained,
            trace_integrity: None,
            copies,
            holds: Vec::new(),
        };
        inventory.validate()?;
        Ok(inventory)
    }

    pub const fn key_id(&self) -> ComplianceKeyId {
        self.key_id
    }

    pub const fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    pub const fn retention_policy(&self) -> RetentionPolicy {
        self.retention_policy
    }

    pub const fn retention_until_ms(&self) -> u64 {
        self.retention_until_ms
    }

    pub const fn state(&self) -> InventoryState {
        self.state
    }

    pub const fn trace_integrity(&self) -> Option<TraceIntegrityEvidence> {
        self.trace_integrity
    }

    pub fn copies(&self) -> &[KeyCopy] {
        &self.copies
    }

    pub fn holds(&self) -> &[LegalHold] {
        &self.holds
    }

    /// Replace a retained inventory declaration only when it is a strict monotonic superset.
    /// Existing committed locations and verified-absence evidence cannot be removed or mutated.
    pub fn update_copies_monotonic(&mut self, proposed: Vec<KeyCopy>) -> Result<usize> {
        let (_, added) =
            self.update_policy_and_copies_monotonic(self.retention_policy, proposed)?;
        Ok(added)
    }

    /// Apply a counsel policy revision and full copy declaration without shortening retention or
    /// removing custody evidence. A changed policy requires a new approval-record commitment.
    pub fn update_policy_and_copies_monotonic(
        &mut self,
        proposed_policy: RetentionPolicy,
        mut proposed: Vec<KeyCopy>,
    ) -> Result<(bool, usize)> {
        if self.state != InventoryState::Retained {
            return Err(ComplianceError::StateConflict);
        }
        proposed_policy.validate()?;
        canonicalize_copies(&mut proposed);
        let policy_changed = proposed_policy != self.retention_policy;
        if proposed.len() < self.copies.len()
            || self
                .copies
                .iter()
                .any(|existing| !proposed.iter().any(|candidate| candidate == existing))
            || (proposed.len() == self.copies.len() && !policy_changed)
            || (policy_changed
                && proposed_policy.counsel_approval_commitment
                    == self.retention_policy.counsel_approval_commitment)
        {
            return Err(ComplianceError::StateConflict);
        }
        let proposed_retention_until = proposed_policy.retention_until(&self.key_id)?;
        if proposed_retention_until < self.retention_until_ms {
            return Err(ComplianceError::StateConflict);
        }
        let added = proposed.len() - self.copies.len();
        let mut candidate = self.clone();
        candidate.retention_policy = proposed_policy;
        candidate.retention_until_ms = proposed_retention_until;
        candidate.copies = proposed;
        candidate.validate()?;
        *self = candidate;
        Ok((policy_changed, added))
    }

    /// Encode the complete hold/destruction state to one strict, bounded binary representation.
    /// Raw custody locators and order references are commitments before they reach this state.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::with_capacity(
            INVENTORY_MAGIC.len()
                + 1
                + COMPLIANCE_KEY_ID_LEN
                + POLICY_ENCODED_LEN
                + 8
                + 8
                + 1
                + 1
                + usize::from(self.trace_integrity.is_some()) * 32
                + 1
                + self.copies.len() * COPY_ENCODED_LEN
                + 1
                + self.holds.len() * (HOLD_RECEIPT_ENCODED_LEN * 2 + 1),
        );
        out.extend_from_slice(INVENTORY_MAGIC);
        out.push(INVENTORY_VERSION);
        out.extend_from_slice(
            &self
                .key_id
                .encode()
                .map_err(|_| ComplianceError::IncompleteInventory)?,
        );
        out.push(self.retention_policy.version);
        out.extend_from_slice(&self.retention_policy.us_days.to_be_bytes());
        out.extend_from_slice(&self.retention_policy.eu_days.to_be_bytes());
        out.extend_from_slice(&self.retention_policy.tr_days.to_be_bytes());
        out.extend_from_slice(&self.retention_policy.test_days.to_be_bytes());
        out.extend_from_slice(&self.retention_policy.counsel_approval_commitment);
        out.extend_from_slice(&self.created_at_ms.to_be_bytes());
        out.extend_from_slice(&self.retention_until_ms.to_be_bytes());
        out.push(inventory_state_byte(self.state));
        match self.trace_integrity {
            None => out.push(0),
            Some(evidence) => {
                out.push(match evidence.status {
                    TraceIntegrityStatus::Verified => 1,
                    TraceIntegrityStatus::Degraded => 2,
                });
                out.extend_from_slice(&evidence.terminal_manifest_commitment);
            }
        }
        out.push(self.copies.len() as u8);
        for copy in &self.copies {
            out.extend_from_slice(&copy.copy_id);
            out.push(copy_kind_byte(copy.kind));
            out.push(copy_state_byte(copy.state));
            push_optional32(&mut out, copy.destruction_evidence);
        }
        out.push(self.holds.len() as u8);
        for hold in &self.holds {
            hold.term_receipt.encode_into(&mut out)?;
            out.push(u8::from(hold.release_receipt.is_some()));
            if let Some(release) = &hold.release_receipt {
                release.encode_into(&mut out)?;
            }
        }
        Ok(out)
    }

    /// Decode exactly one inventory. Unknown versions/discriminants, inconsistent states, and
    /// trailing bytes are rejected before the value can participate in a hold/shred transition.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let minimum_v2 = INVENTORY_MAGIC.len()
            + 1
            + COMPLIANCE_KEY_ID_LEN
            + POLICY_ENCODED_LEN
            + 8
            + 8
            + 1
            + 1
            + 1;
        if bytes.len() < minimum_v2 || &bytes[..INVENTORY_MAGIC.len()] != INVENTORY_MAGIC {
            return Err(ComplianceError::IncompleteInventory);
        }
        let version = bytes[INVENTORY_MAGIC.len()];
        if !matches!(version, LEGACY_INVENTORY_VERSION | INVENTORY_VERSION) {
            return Err(ComplianceError::IncompleteInventory);
        }
        let mut reader = InventoryReader::new(bytes, INVENTORY_MAGIC.len() + 1);
        let key_id = ComplianceKeyId::decode(reader.take(COMPLIANCE_KEY_ID_LEN)?)
            .map_err(|_| ComplianceError::IncompleteInventory)?;
        let retention_policy = RetentionPolicy {
            version: reader.u8()?,
            us_days: reader.u16()?,
            eu_days: reader.u16()?,
            tr_days: reader.u16()?,
            test_days: reader.u16()?,
            counsel_approval_commitment: reader.array32()?,
        };
        let created_at_ms = reader.u64()?;
        let retention_until_ms = reader.u64()?;
        let state = inventory_state_from_byte(reader.u8()?)?;
        let trace_integrity = if version == INVENTORY_VERSION {
            match reader.u8()? {
                0 => None,
                status @ (1 | 2) => Some(TraceIntegrityEvidence::new(
                    if status == 1 {
                        TraceIntegrityStatus::Verified
                    } else {
                        TraceIntegrityStatus::Degraded
                    },
                    reader.array32()?,
                )?),
                _ => return Err(ComplianceError::IncompleteInventory),
            }
        } else {
            None
        };
        let copy_count = reader.u8()? as usize;
        if copy_count == 0 || copy_count > MAX_INVENTORY_COPIES {
            return Err(ComplianceError::IncompleteInventory);
        }
        let mut copies = Vec::with_capacity(copy_count);
        for _ in 0..copy_count {
            copies.push(KeyCopy {
                copy_id: reader.array32()?,
                kind: copy_kind_from_byte(reader.u8()?)?,
                state: copy_state_from_byte(reader.u8()?)?,
                destruction_evidence: reader.optional32()?,
            });
        }
        let hold_count = reader.u8()? as usize;
        if hold_count > MAX_INVENTORY_HOLDS {
            return Err(ComplianceError::IncompleteInventory);
        }
        let mut holds = Vec::with_capacity(hold_count);
        for _ in 0..hold_count {
            let term_receipt = LegalHoldReceipt::decode(&mut reader)?;
            let release_receipt = match reader.u8()? {
                0 => None,
                1 => Some(LegalHoldReceipt::decode(&mut reader)?),
                _ => return Err(ComplianceError::IncompleteInventory),
            };
            holds.push(LegalHold {
                term_receipt,
                release_receipt,
            });
        }
        if !reader.finished() {
            return Err(ComplianceError::IncompleteInventory);
        }
        let inventory = Self {
            key_id,
            retention_policy,
            created_at_ms,
            retention_until_ms,
            state,
            trace_integrity,
            copies,
            holds,
        };
        inventory.validate()?;
        Ok(inventory)
    }

    /// Place a renewable hold. Each term is capped at 90 days, including the US requirement.
    pub(crate) fn place_hold(
        &mut self,
        starts_at_ms: u64,
        expires_at_ms: u64,
        authorization: LegalHoldAuthorization,
    ) -> Result<[u8; 32]> {
        self.place_hold_inner(starts_at_ms, expires_at_ms, None, authorization)
    }

    pub(crate) fn renew_hold(
        &mut self,
        prior_hold_id: [u8; 32],
        starts_at_ms: u64,
        expires_at_ms: u64,
        authorization: LegalHoldAuthorization,
    ) -> Result<[u8; 32]> {
        let prior = self
            .holds
            .iter()
            .find(|hold| hold.hold_id() == prior_hold_id)
            .ok_or(ComplianceError::StateConflict)?;
        if !prior.active_at(starts_at_ms)
            || self
                .holds
                .iter()
                .any(|hold| hold.renews() == Some(prior_hold_id))
        {
            return Err(ComplianceError::StateConflict);
        }
        self.place_hold_inner(
            starts_at_ms,
            expires_at_ms,
            Some((prior_hold_id, prior.term_receipt.receipt_hash()?)),
            authorization,
        )
    }

    fn place_hold_inner(
        &mut self,
        starts_at_ms: u64,
        expires_at_ms: u64,
        predecessor: Option<([u8; 32], [u8; 32])>,
        authorization: LegalHoldAuthorization,
    ) -> Result<[u8; 32]> {
        if self.state != InventoryState::Retained || self.holds.len() >= MAX_INVENTORY_HOLDS {
            return Err(ComplianceError::StateConflict);
        }
        let term = expires_at_ms
            .checked_sub(starts_at_ms)
            .ok_or(ComplianceError::InvalidRequest)?;
        if starts_at_ms == 0 || term == 0 || term > MAX_HOLD_TERM_MS {
            return Err(ComplianceError::InvalidRequest);
        }
        let predecessor_hold_id = predecessor.map(|value| value.0);
        let predecessor_receipt_hash = predecessor.map(|value| value.1);
        let action = if predecessor.is_some() {
            LegalHoldReceiptAction::Renew
        } else {
            LegalHoldReceiptAction::Place
        };
        let hold_id = canonical_hold_id(
            &self.key_id,
            starts_at_ms,
            expires_at_ms,
            predecessor_hold_id,
            &authorization,
        )?;
        let receipt = LegalHoldReceipt {
            action,
            hold_id,
            key_id: self.key_id,
            starts_at_ms,
            expires_at_ms,
            acted_at_ms: authorization.created_at_ms,
            predecessor_hold_id,
            predecessor_receipt_hash,
            authorization,
        };
        receipt.validate()?;
        let mut candidate = self.clone();
        candidate.holds.push(LegalHold {
            term_receipt: receipt,
            release_receipt: None,
        });
        candidate.validate()?;
        *self = candidate;
        Ok(hold_id)
    }

    pub(crate) fn release_hold(
        &mut self,
        hold_id: [u8; 32],
        released_at_ms: u64,
        authorization: LegalHoldAuthorization,
    ) -> Result<()> {
        if self.state != InventoryState::Retained {
            return Err(ComplianceError::StateConflict);
        }
        let index = self
            .holds
            .iter()
            .position(|hold| hold.hold_id() == hold_id)
            .ok_or(ComplianceError::StateConflict)?;
        let hold = &self.holds[index];
        if !hold.active_at(released_at_ms) || hold.release_receipt.is_some() {
            return Err(ComplianceError::StateConflict);
        }
        let receipt = LegalHoldReceipt {
            action: LegalHoldReceiptAction::Release,
            hold_id,
            key_id: self.key_id,
            starts_at_ms: hold.starts_at_ms(),
            expires_at_ms: hold.expires_at_ms(),
            acted_at_ms: authorization.created_at_ms,
            predecessor_hold_id: Some(hold_id),
            predecessor_receipt_hash: Some(hold.term_receipt.receipt_hash()?),
            authorization,
        };
        if released_at_ms != receipt.acted_at_ms {
            return Err(ComplianceError::InvalidRequest);
        }
        receipt.validate()?;
        let mut candidate = self.clone();
        candidate.holds[index].release_receipt = Some(receipt);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Persist the terminal-manifest commitment and ciphertext integrity observed for a trace
    /// epoch before destruction. Evidence is monotonic: a degradation can never be erased, and a
    /// different signed terminal manifest can never be substituted during a resumed ceremony.
    pub(crate) fn record_trace_integrity(
        &mut self,
        evidence: TraceIntegrityEvidence,
    ) -> Result<()> {
        if self.state == InventoryState::Shredded
            || evidence.terminal_manifest_commitment == [0u8; 32]
        {
            return Err(ComplianceError::StateConflict);
        }
        self.trace_integrity = match self.trace_integrity {
            None => Some(evidence),
            Some(existing)
                if existing.terminal_manifest_commitment
                    == evidence.terminal_manifest_commitment =>
            {
                Some(TraceIntegrityEvidence {
                    status: if matches!(
                        (existing.status, evidence.status),
                        (TraceIntegrityStatus::Degraded, _) | (_, TraceIntegrityStatus::Degraded)
                    ) {
                        TraceIntegrityStatus::Degraded
                    } else {
                        TraceIntegrityStatus::Verified
                    },
                    terminal_manifest_commitment: existing.terminal_manifest_commitment,
                })
            }
            Some(_) => return Err(ComplianceError::StateConflict),
        };
        Ok(())
    }

    /// Begin destruction only after retention and every active legal hold have cleared.
    pub fn begin_shred(&mut self, at_ms: u64) -> Result<()> {
        if self.state != InventoryState::Retained {
            return Err(ComplianceError::StateConflict);
        }
        let epoch_end_ms = compliance_epoch_end_ms(&self.key_id)
            .map_err(|_| ComplianceError::IncompleteInventory)?;
        if at_ms < epoch_end_ms || at_ms < self.retention_until_ms {
            return Err(ComplianceError::RetentionActive);
        }
        if self.holds.iter().any(|hold| hold.active_at(at_ms)) {
            return Err(ComplianceError::LegalHoldActive);
        }
        self.state = InventoryState::Shredding;
        for copy in &mut self.copies {
            if copy.state == CopyState::Present {
                copy.state = CopyState::DestructionRequested;
            }
        }
        Ok(())
    }

    /// Record a deletion receipt commitment from a storage/KMS/share custodian.
    pub fn record_destroyed(
        &mut self,
        copy_id: [u8; 32],
        receipt_commitment: [u8; 32],
    ) -> Result<()> {
        if self.state != InventoryState::Shredding || receipt_commitment == [0u8; 32] {
            return Err(ComplianceError::StateConflict);
        }
        let copy = self
            .copies
            .iter_mut()
            .find(|copy| copy.copy_id == copy_id)
            .ok_or(ComplianceError::UnknownCopy)?;
        if copy.state != CopyState::DestructionRequested {
            return Err(ComplianceError::StateConflict);
        }
        copy.state = CopyState::Destroyed;
        copy.destruction_evidence = Some(receipt_commitment);
        Ok(())
    }

    pub fn record_verified_absent(
        &mut self,
        copy_id: [u8; 32],
        evidence_commitment: [u8; 32],
    ) -> Result<()> {
        if self.state != InventoryState::Shredding || evidence_commitment == [0u8; 32] {
            return Err(ComplianceError::StateConflict);
        }
        let copy = self
            .copies
            .iter_mut()
            .find(|copy| copy.copy_id == copy_id)
            .ok_or(ComplianceError::UnknownCopy)?;
        if copy.state != CopyState::DestructionRequested {
            return Err(ComplianceError::StateConflict);
        }
        copy.state = CopyState::VerifiedAbsent;
        copy.destruction_evidence = Some(evidence_commitment);
        Ok(())
    }

    /// Complete only when every declared location is destroyed or independently verified absent.
    pub fn complete_shred(&mut self) -> Result<()> {
        if self.state != InventoryState::Shredding
            || self.copies.iter().any(|copy| {
                !matches!(copy.state, CopyState::Destroyed | CopyState::VerifiedAbsent)
                    || copy.destruction_evidence.is_none()
            })
        {
            return Err(ComplianceError::IncompleteInventory);
        }
        self.state = InventoryState::Shredded;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        self.retention_policy.validate()?;
        let required_retention = self.retention_policy.retention_until(&self.key_id)?;
        if self.key_id.validate().is_err()
            || compliance_epoch_end_ms(&self.key_id).is_err()
            || self.created_at_ms != self.key_id.epoch_start_ms
            || self.retention_until_ms != required_retention
            || self.copies.is_empty()
            || self.copies.len() > MAX_INVENTORY_COPIES
            || self.holds.len() > MAX_INVENTORY_HOLDS
            || self
                .trace_integrity
                .is_some_and(|evidence| evidence.terminal_manifest_commitment == [0u8; 32])
            || REQUIRED_COPY_KINDS
                .iter()
                .any(|kind| !self.copies.iter().any(|copy| copy.kind == *kind))
        {
            return Err(ComplianceError::IncompleteInventory);
        }
        let mut copy_ids: Vec<[u8; 32]> = self.copies.iter().map(|copy| copy.copy_id).collect();
        copy_ids.sort_unstable();
        if copy_ids.contains(&[0u8; 32])
            || copy_ids.windows(2).any(|pair| pair[0] == pair[1])
            || self
                .copies
                .windows(2)
                .any(|pair| (pair[0].kind, pair[0].copy_id) >= (pair[1].kind, pair[1].copy_id))
        {
            return Err(ComplianceError::IncompleteInventory);
        }
        for copy in &self.copies {
            let evidence_valid = match copy.state {
                CopyState::Present | CopyState::DestructionRequested => {
                    copy.destruction_evidence.is_none()
                }
                CopyState::Destroyed | CopyState::VerifiedAbsent => copy
                    .destruction_evidence
                    .is_some_and(|commitment| commitment != [0u8; 32]),
            };
            let state_valid = match self.state {
                InventoryState::Retained => {
                    matches!(copy.state, CopyState::Present | CopyState::VerifiedAbsent)
                }
                InventoryState::Shredding => !matches!(copy.state, CopyState::Present),
                InventoryState::Shredded => {
                    matches!(copy.state, CopyState::Destroyed | CopyState::VerifiedAbsent)
                }
            };
            if !evidence_valid || !state_valid {
                return Err(ComplianceError::IncompleteInventory);
            }
        }
        let mut hold_ids = Vec::with_capacity(self.holds.len());
        let mut child_predecessors = Vec::with_capacity(self.holds.len());
        let mut hold_request_ids = Vec::with_capacity(self.holds.len() * 2);
        for hold in &self.holds {
            hold.term_receipt.validate()?;
            let hold_id = hold.hold_id();
            if hold.term_receipt.key_id != self.key_id
                || !matches!(
                    hold.term_receipt.action,
                    LegalHoldReceiptAction::Place | LegalHoldReceiptAction::Renew
                )
                || hold_ids.contains(&hold_id)
                || hold_request_ids.contains(&hold.term_receipt.authorization.request_id)
            {
                return Err(ComplianceError::IncompleteInventory);
            }
            if let Some(predecessor_id) = hold.renews() {
                let predecessor = self
                    .holds
                    .iter()
                    .take(hold_ids.len())
                    .find(|candidate| candidate.hold_id() == predecessor_id)
                    .ok_or(ComplianceError::IncompleteInventory)?;
                if child_predecessors.contains(&predecessor_id)
                    || !predecessor.active_at(hold.starts_at_ms())
                    || hold.term_receipt.predecessor_receipt_hash
                        != Some(predecessor.term_receipt.receipt_hash()?)
                {
                    return Err(ComplianceError::IncompleteInventory);
                }
                child_predecessors.push(predecessor_id);
            }
            hold_request_ids.push(hold.term_receipt.authorization.request_id);
            if let Some(release) = &hold.release_receipt {
                release.validate()?;
                if release.action != LegalHoldReceiptAction::Release
                    || release.key_id != self.key_id
                    || release.hold_id != hold_id
                    || release.starts_at_ms != hold.starts_at_ms()
                    || release.expires_at_ms != hold.expires_at_ms()
                    || release.acted_at_ms < hold.starts_at_ms()
                    || release.acted_at_ms > hold.expires_at_ms()
                    || release.predecessor_receipt_hash != Some(hold.term_receipt.receipt_hash()?)
                    || hold_request_ids.contains(&release.authorization.request_id)
                {
                    return Err(ComplianceError::IncompleteInventory);
                }
                hold_request_ids.push(release.authorization.request_id);
            }
            hold_ids.push(hold_id);
        }
        Ok(())
    }
}

fn hold_selector_material(
    action: LegalHoldReceiptAction,
    hold_id: [u8; 32],
    expires_at_ms: u64,
    predecessor_hold_id: Option<[u8; 32]>,
) -> Result<Vec<u8>> {
    let mut material = Vec::with_capacity(5 + 32 + 8);
    match action {
        LegalHoldReceiptAction::Place => {
            if predecessor_hold_id.is_some() {
                return Err(ComplianceError::IncompleteInventory);
            }
            material.extend_from_slice(b"place");
            material.extend_from_slice(&expires_at_ms.to_be_bytes());
        }
        LegalHoldReceiptAction::Renew => {
            material.extend_from_slice(b"renew");
            material.extend_from_slice(
                &predecessor_hold_id.ok_or(ComplianceError::IncompleteInventory)?,
            );
            material.extend_from_slice(&expires_at_ms.to_be_bytes());
        }
        LegalHoldReceiptAction::Release => {
            material.extend_from_slice(b"release");
            material.extend_from_slice(&hold_id);
        }
    }
    Ok(material)
}

fn canonical_hold_id(
    key_id: &ComplianceKeyId,
    starts_at_ms: u64,
    expires_at_ms: u64,
    predecessor_hold_id: Option<[u8; 32]>,
    authorization: &LegalHoldAuthorization,
) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(HOLD_ID_DOMAIN);
    hash.update(
        key_id
            .encode()
            .map_err(|_| ComplianceError::IncompleteInventory)?,
    );
    hash.update(starts_at_ms.to_be_bytes());
    hash.update(expires_at_ms.to_be_bytes());
    hash.update([u8::from(predecessor_hold_id.is_some())]);
    hash.update(predecessor_hold_id.unwrap_or([0u8; 32]));
    hash.update(authorization.request_id);
    hash.update(authorization.order_commitment);
    let hold_id: [u8; 32] = hash.finalize().into();
    if hold_id == [0u8; 32] {
        return Err(ComplianceError::IncompleteInventory);
    }
    Ok(hold_id)
}

fn disclosure_request_id(
    key_id: &ComplianceKeyId,
    created_at_ms: u64,
    expires_at_ms: u64,
    order_commitment: &[u8; 32],
    requester_commitment: &[u8; 32],
    selector_commitment: &[u8; 32],
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(DISCLOSURE_REQUEST_DOMAIN);
    hash.update([key_id.jurisdiction.into(), key_id.purpose.into()]);
    hash.update(created_at_ms.to_be_bytes());
    hash.update(expires_at_ms.to_be_bytes());
    hash.update([1]);
    hash.update(key_id.encode().expect("validated receipt key id"));
    hash.update(order_commitment);
    hash.update(requester_commitment);
    hash.update(selector_commitment);
    hash.finalize().into()
}

fn disclosure_approval_preimage(request_id: &[u8; 32], approved_at_ms: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(DISCLOSURE_APPROVAL_DOMAIN.len() + 32 + 8 + 1);
    out.extend_from_slice(DISCLOSURE_APPROVAL_DOMAIN);
    out.extend_from_slice(request_id);
    out.extend_from_slice(&approved_at_ms.to_be_bytes());
    out.push(1);
    out
}

fn disclosure_commitment(label: &[u8], salt: &[u8; 32], raw: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(DISCLOSURE_COMMITMENT_DOMAIN);
    hash.update((label.len() as u16).to_be_bytes());
    hash.update(label);
    hash.update(salt);
    hash.update((raw.len() as u64).to_be_bytes());
    hash.update(raw);
    hash.finalize().into()
}

fn hold_approver_key_id(public_key: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(HOLD_APPROVER_KEY_ID_DOMAIN);
    hash.update(public_key);
    hash.finalize().into()
}

fn copy_kind_byte(value: KeyCopyKind) -> u8 {
    value as u8
}

fn copy_kind_from_byte(value: u8) -> Result<KeyCopyKind> {
    match value {
        1 => Ok(KeyCopyKind::LiveMetadata),
        2 => Ok(KeyCopyKind::SqliteWal),
        3 => Ok(KeyCopyKind::Sidecar),
        4 => Ok(KeyCopyKind::Snapshot),
        5 => Ok(KeyCopyKind::Backup),
        6 => Ok(KeyCopyKind::KmsVersion),
        7 => Ok(KeyCopyKind::ShamirShare),
        _ => Err(ComplianceError::IncompleteInventory),
    }
}

fn copy_state_byte(value: CopyState) -> u8 {
    match value {
        CopyState::Present => 1,
        CopyState::DestructionRequested => 2,
        CopyState::Destroyed => 3,
        CopyState::VerifiedAbsent => 4,
    }
}

fn copy_state_from_byte(value: u8) -> Result<CopyState> {
    match value {
        1 => Ok(CopyState::Present),
        2 => Ok(CopyState::DestructionRequested),
        3 => Ok(CopyState::Destroyed),
        4 => Ok(CopyState::VerifiedAbsent),
        _ => Err(ComplianceError::IncompleteInventory),
    }
}

fn inventory_state_byte(value: InventoryState) -> u8 {
    match value {
        InventoryState::Retained => 1,
        InventoryState::Shredding => 2,
        InventoryState::Shredded => 3,
    }
}

fn inventory_state_from_byte(value: u8) -> Result<InventoryState> {
    match value {
        1 => Ok(InventoryState::Retained),
        2 => Ok(InventoryState::Shredding),
        3 => Ok(InventoryState::Shredded),
        _ => Err(ComplianceError::IncompleteInventory),
    }
}

fn push_optional32(out: &mut Vec<u8>, value: Option<[u8; 32]>) {
    out.push(u8::from(value.is_some()));
    out.extend_from_slice(&value.unwrap_or([0u8; 32]));
}

struct InventoryReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> InventoryReader<'a> {
    const fn new(bytes: &'a [u8], cursor: usize) -> Self {
        Self { bytes, cursor }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ComplianceError::IncompleteInventory)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ComplianceError::IncompleteInventory)?;
        self.cursor = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("fixed slice"),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("fixed slice"),
        ))
    }

    fn array32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().expect("fixed slice"))
    }

    fn array64(&mut self) -> Result<[u8; 64]> {
        Ok(self.take(64)?.try_into().expect("fixed slice"))
    }

    fn optional32(&mut self) -> Result<Option<[u8; 32]>> {
        let present = self.u8()?;
        let value = self.array32()?;
        match (present, value) {
            (0, value) if value == [0u8; 32] => Ok(None),
            (1, value) if value != [0u8; 32] => Ok(Some(value)),
            _ => Err(ComplianceError::IncompleteInventory),
        }
    }

    fn finished(&self) -> bool {
        self.cursor == self.bytes.len()
    }
}

fn canonicalize_copies(copies: &mut [KeyCopy]) {
    copies.sort_unstable_by_key(|copy| (copy.kind, copy.copy_id));
}

fn random_nonzero_nonce() -> [u8; 32] {
    loop {
        let mut nonce = [0u8; 32];
        OsRng.fill_bytes(&mut nonce);
        if nonce != [0u8; 32] {
            return nonce;
        }
    }
}

fn copy_commitment(
    domain: &[u8],
    kind: KeyCopyKind,
    nonce: [u8; 32],
    raw: &[u8],
) -> Result<[u8; 32]> {
    if raw.is_empty() || nonce == [0u8; 32] {
        return Err(ComplianceError::IncompleteInventory);
    }
    let commitment = commitment(domain, kind as u8, &nonce, raw);
    if commitment == [0u8; 32] {
        return Err(ComplianceError::IncompleteInventory);
    }
    Ok(commitment)
}

fn commitment(domain: &[u8], discriminator: u8, nonce: &[u8; 32], raw: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update([discriminator]);
    hash.update(nonce);
    hash.update((raw.len() as u64).to_be_bytes());
    hash.update(raw);
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};
    use pigeonpost_compliance_format::CompliancePurpose;

    use super::*;
    use crate::approval::{DisclosureRequest, SensitiveRequestMaterial};

    fn all_copies() -> Vec<KeyCopy> {
        REQUIRED_COPY_KINDS
            .iter()
            .map(|kind| {
                if *kind == KeyCopyKind::LiveMetadata || *kind == KeyCopyKind::Backup {
                    KeyCopy::present(*kind, format!("private-{kind:?}").as_bytes()).unwrap()
                } else {
                    KeyCopy::verified_absent(*kind, format!("checked-{kind:?}").as_bytes()).unwrap()
                }
            })
            .collect()
    }

    fn policy() -> RetentionPolicy {
        RetentionPolicy::new(TR_RETENTION_DAYS_MIN, [42; 32]).unwrap()
    }

    fn inventory() -> DestructionInventory {
        DestructionInventory::new(
            ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [1; 32],
                0,
                1,
            ),
            0,
            policy(),
            all_copies(),
        )
        .unwrap()
    }

    fn hold_authorization(
        key_id: ComplianceKeyId,
        order_reference: &[u8],
        action: LegalHoldReceiptAction,
        hold_id: [u8; 32],
        predecessor_hold_id: Option<[u8; 32]>,
        at_ms: u64,
        hold_expires_at_ms: u64,
    ) -> LegalHoldAuthorization {
        let selector =
            hold_selector_material(action, hold_id, hold_expires_at_ms, predecessor_hold_id)
                .unwrap();
        let (mut request, openings) = DisclosureRequest::new(
            key_id.jurisdiction,
            key_id.purpose,
            vec![key_id],
            at_ms,
            at_ms + 1_000,
            SensitiveRequestMaterial {
                order_reference,
                requester_identity: LEGAL_PROCESS_REQUESTER,
                selectors: &selector,
            },
        )
        .unwrap();
        let signers = [
            SigningKey::from_bytes(&[81; 32]),
            SigningKey::from_bytes(&[82; 32]),
        ];
        let mut approvals = Vec::with_capacity(2);
        for signer in &signers {
            let signature = signer.sign(&request.approval_preimage(at_ms)).to_bytes();
            let public_key = signer.verifying_key().to_bytes();
            request.approve(public_key, at_ms, signature).unwrap();
            approvals.push(LegalHoldApproval::new(public_key, at_ms, signature).unwrap());
        }
        LegalHoldAuthorization::from_request(&request, &openings, approvals.try_into().unwrap())
            .unwrap()
    }

    fn place_hold(
        inventory: &mut DestructionInventory,
        order_reference: &[u8],
        starts_at_ms: u64,
        expires_at_ms: u64,
    ) -> [u8; 32] {
        let authorization = hold_authorization(
            inventory.key_id(),
            order_reference,
            LegalHoldReceiptAction::Place,
            [1; 32],
            None,
            starts_at_ms,
            expires_at_ms,
        );
        inventory
            .place_hold(starts_at_ms, expires_at_ms, authorization)
            .unwrap()
    }

    #[test]
    fn versioned_policy_encodes_explicit_product_choices_and_counsel_selected_tr_days() {
        let policy = RetentionPolicy::new(548, [9; 32]).unwrap();
        let key = |jurisdiction| {
            ComplianceKeyId::new(CompliancePurpose::NetworkTrace, jurisdiction, [9; 32], 0, 1)
        };
        assert_eq!(
            policy.retention_until(&key(Jurisdiction::Eu)).unwrap(),
            DAY_MS
        );
        assert_eq!(
            policy.retention_until(&key(Jurisdiction::Us)).unwrap(),
            31 * DAY_MS
        );
        assert_eq!(
            policy.retention_until(&key(Jurisdiction::Tr)).unwrap(),
            549 * DAY_MS
        );
        assert_eq!(
            policy.retention_until(&key(Jurisdiction::Test)).unwrap(),
            2 * DAY_MS
        );
        assert!(RetentionPolicy::new(TR_RETENTION_DAYS_MIN - 1, [9; 32]).is_err());
        assert!(RetentionPolicy::new(TR_RETENTION_DAYS_MAX + 1, [9; 32]).is_err());
        assert!(RetentionPolicy::new(TR_RETENTION_DAYS_MIN, [0; 32]).is_err());
    }

    #[test]
    fn trace_inventories_require_the_canonical_daily_epoch() {
        let unaligned = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [1; 32],
            1,
            1,
        );
        assert!(matches!(
            DestructionInventory::new(unaligned, 1, policy(), all_copies()),
            Err(ComplianceError::IncompleteInventory)
        ));
    }

    #[test]
    fn eu_zero_day_attribution_retention_starts_after_the_leap_month_closes() {
        const FEBRUARY_2024: u64 = 1_706_745_600_000;
        const MARCH_2024: u64 = 1_709_251_200_000;
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Eu,
            [7; 32],
            FEBRUARY_2024,
            1,
        );
        let mut inventory =
            DestructionInventory::new(key_id, FEBRUARY_2024, policy(), all_copies()).unwrap();
        assert_eq!(inventory.retention_until_ms(), MARCH_2024);
        assert_eq!(
            inventory.begin_shred(MARCH_2024 - 1),
            Err(ComplianceError::RetentionActive)
        );
        inventory.begin_shred(MARCH_2024).unwrap();
    }

    #[test]
    fn late_daily_records_keep_the_full_post_epoch_us_retention_window() {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Us,
            [7; 32],
            0,
            1,
        );
        let mut inventory = DestructionInventory::new(key_id, 0, policy(), all_copies()).unwrap();
        assert_eq!(inventory.retention_until_ms(), 31 * DAY_MS);
        assert_eq!(
            inventory.begin_shred(30 * DAY_MS + (DAY_MS - 1)),
            Err(ComplianceError::RetentionActive)
        );
        inventory.begin_shred(31 * DAY_MS).unwrap();
    }

    #[test]
    fn active_hold_and_shredding_are_serialized() {
        let mut held_inventory = inventory();
        place_hold(&mut held_inventory, b"private-order", 2, 3 * DAY_MS);
        assert_eq!(
            held_inventory.begin_shred(2 * DAY_MS + 2),
            Err(ComplianceError::LegalHoldActive)
        );
        let mut other = inventory();
        other.begin_shred(2 * DAY_MS + 2).unwrap();
        let authorization = hold_authorization(
            other.key_id(),
            b"late-order",
            LegalHoldReceiptAction::Place,
            [1; 32],
            None,
            2 * DAY_MS + 2,
            2 * DAY_MS + 3,
        );
        assert_eq!(
            other.place_hold(2 * DAY_MS + 2, 2 * DAY_MS + 3, authorization),
            Err(ComplianceError::StateConflict)
        );
    }

    #[test]
    fn backup_blocks_completion_until_receipted() {
        let mut inventory = inventory();
        inventory.begin_shred(2 * DAY_MS + 2).unwrap();
        let requested: Vec<[u8; 32]> = inventory
            .copies()
            .iter()
            .filter(|copy| copy.state() == CopyState::DestructionRequested)
            .map(KeyCopy::copy_id)
            .collect();
        assert_eq!(requested.len(), 2);
        inventory.record_destroyed(requested[0], [7; 32]).unwrap();
        assert_eq!(
            inventory.complete_shred(),
            Err(ComplianceError::IncompleteInventory)
        );
        inventory.record_destroyed(requested[1], [8; 32]).unwrap();
        inventory.complete_shred().unwrap();
        assert_eq!(inventory.state(), InventoryState::Shredded);
    }

    #[test]
    fn every_storage_class_must_be_explicit() {
        let mut copies = all_copies();
        copies.retain(|copy| copy.kind() != KeyCopyKind::ShamirShare);
        assert!(DestructionInventory::new(
            ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [1; 32],
                1,
                1,
            ),
            1,
            policy(),
            copies,
        )
        .is_err());
    }

    #[test]
    fn raw_locations_and_orders_are_not_debugged() {
        let mut inventory = inventory();
        place_hold(&mut inventory, b"private-order-number", 2, 3);
        let debug = format!("{inventory:?}");
        assert!(!debug.contains("private-order-number"));
        assert!(!debug.contains("private-LiveMetadata"));
    }

    #[test]
    fn inventory_codec_is_exact_and_preserves_hold_and_shred_state() {
        let mut retained = inventory();
        place_hold(&mut retained, b"private-order", 2, 3);
        let encoded = retained.encode().unwrap();
        let decoded = DestructionInventory::decode(&encoded).unwrap();
        assert_eq!(decoded.key_id(), retained.key_id());
        assert_eq!(decoded.state(), InventoryState::Retained);
        assert_eq!(decoded.holds(), retained.holds());

        let mut extended = encoded.clone();
        extended.push(0);
        assert!(matches!(
            DestructionInventory::decode(&extended),
            Err(ComplianceError::IncompleteInventory)
        ));

        let mut shredding = inventory();
        shredding.begin_shred(2 * DAY_MS + 2).unwrap();
        let decoded = DestructionInventory::decode(&shredding.encode().unwrap()).unwrap();
        assert_eq!(decoded.state(), InventoryState::Shredding);
        assert!(decoded
            .copies()
            .iter()
            .any(|copy| copy.state() == CopyState::DestructionRequested));
    }

    #[test]
    fn signed_hold_receipts_survive_restart_and_reject_signature_tampering() {
        let mut inventory = inventory();
        let key_id = inventory.key_id();
        let authorization = hold_authorization(
            key_id,
            b"private-order",
            LegalHoldReceiptAction::Place,
            [1; 32],
            None,
            2,
            100,
        );
        let hold_id = inventory.place_hold(2, 100, authorization).unwrap();
        let hold = &inventory.holds()[0];
        assert_eq!(hold.hold_id(), hold_id);
        assert_ne!(hold.term_receipt().order_commitment(), [0; 32]);
        assert_eq!(hold.term_receipt().key_id(), key_id);
        assert_ne!(hold.term_receipt().request_id(), [0; 32]);
        assert_eq!(hold.term_receipt().approval_timestamps_ms(), [2, 2]);
        let roster = hold.term_receipt().approval_roster();
        assert_ne!(roster[0].0, roster[1].0);
        assert_ne!(roster[0].1, roster[1].1);
        assert_eq!(hold.term_receipt().acted_at_ms(), 2);
        assert!(hold.term_receipt().predecessor_hold_id().is_none());
        assert!(hold.term_receipt().predecessor_receipt_hash().is_none());

        let encoded = inventory.encode().unwrap();
        let decoded = DestructionInventory::decode(&encoded).unwrap();
        assert_eq!(decoded.holds(), inventory.holds());
        assert_eq!(
            decoded.holds()[0].term_receipt().receipt_hash().unwrap(),
            inventory.holds()[0].term_receipt().receipt_hash().unwrap()
        );

        let mut tampered = encoded;
        let signature_byte = tampered.len() - 2;
        tampered[signature_byte] ^= 1;
        assert!(DestructionInventory::decode(&tampered).is_err());
    }

    #[test]
    fn hold_lineage_is_linear_active_and_non_replayable() {
        let mut inventory = inventory();
        let key_id = inventory.key_id();
        let first = place_hold(&mut inventory, b"first-order", 2, 100);

        let renewal_authorization = hold_authorization(
            key_id,
            b"renewal-order",
            LegalHoldReceiptAction::Renew,
            [1; 32],
            Some(first),
            50,
            150,
        );
        let renewed = inventory
            .renew_hold(first, 50, 150, renewal_authorization)
            .unwrap();
        assert_eq!(inventory.holds()[1].renews(), Some(first));
        assert_eq!(
            inventory.holds()[1]
                .term_receipt()
                .predecessor_receipt_hash(),
            Some(inventory.holds()[0].term_receipt().receipt_hash().unwrap())
        );

        let branch_authorization = hold_authorization(
            key_id,
            b"branch-order",
            LegalHoldReceiptAction::Renew,
            [1; 32],
            Some(first),
            60,
            160,
        );
        assert_eq!(
            inventory.renew_hold(first, 60, 160, branch_authorization),
            Err(ComplianceError::StateConflict)
        );

        let release_authorization = hold_authorization(
            key_id,
            b"release-order",
            LegalHoldReceiptAction::Release,
            renewed,
            None,
            100,
            150,
        );
        inventory
            .release_hold(renewed, 100, release_authorization)
            .unwrap();
        assert_eq!(inventory.holds()[1].released_at_ms(), Some(100));

        let after_release = hold_authorization(
            key_id,
            b"late-renewal",
            LegalHoldReceiptAction::Renew,
            [1; 32],
            Some(renewed),
            101,
            180,
        );
        assert_eq!(
            inventory.renew_hold(renewed, 101, 180, after_release),
            Err(ComplianceError::StateConflict)
        );

        let expired = place_hold(&mut inventory, b"short-order", 200, 220);
        let after_expiry = hold_authorization(
            key_id,
            b"expired-renewal",
            LegalHoldReceiptAction::Renew,
            [1; 32],
            Some(expired),
            221,
            300,
        );
        assert_eq!(
            inventory.renew_hold(expired, 221, 300, after_expiry),
            Err(ComplianceError::StateConflict)
        );

        let mut replayed = inventory.clone();
        replayed.holds.push(replayed.holds[0].clone());
        assert!(replayed.encode().is_err());
    }

    #[test]
    fn inventory_policy_codec_rejects_legacy_unknown_truncated_and_tampered_choices() {
        let encoded = inventory().encode().unwrap();

        let mut legacy = encoded.clone();
        legacy[INVENTORY_MAGIC.len()] = 1;
        assert!(matches!(
            DestructionInventory::decode(&legacy),
            Err(ComplianceError::IncompleteInventory)
        ));

        let policy_offset = INVENTORY_MAGIC.len() + 1 + COMPLIANCE_KEY_ID_LEN;
        let mut unknown_policy = encoded.clone();
        unknown_policy[policy_offset] = 99;
        assert!(matches!(
            DestructionInventory::decode(&unknown_policy),
            Err(ComplianceError::IncompleteInventory)
        ));

        let mut changed_us_choice = encoded.clone();
        changed_us_choice[policy_offset + 1..policy_offset + 3]
            .copy_from_slice(&31u16.to_be_bytes());
        assert!(matches!(
            DestructionInventory::decode(&changed_us_choice),
            Err(ComplianceError::IncompleteInventory)
        ));

        assert!(matches!(
            DestructionInventory::decode(&encoded[..encoded.len() - 1]),
            Err(ComplianceError::IncompleteInventory)
        ));

        // A canonical v2 inventory is accepted and upgraded in memory with no trace-integrity
        // evidence. Pre-fix v2 state that used epoch-start-based retention remains invalid and must
        // be recreated/imported rather than silently reinterpreted with a longer expiry.
        let state_offset =
            INVENTORY_MAGIC.len() + 1 + COMPLIANCE_KEY_ID_LEN + POLICY_ENCODED_LEN + 8 + 8;
        let trace_integrity_offset = state_offset + 1;
        assert_eq!(encoded[trace_integrity_offset], 0);
        let mut canonical_v2 = encoded.clone();
        canonical_v2[INVENTORY_MAGIC.len()] = LEGACY_INVENTORY_VERSION;
        canonical_v2.remove(trace_integrity_offset);
        let decoded_v2 = DestructionInventory::decode(&canonical_v2).unwrap();
        assert_eq!(decoded_v2.trace_integrity(), None);
        assert_eq!(
            DestructionInventory::decode(&decoded_v2.encode().unwrap())
                .unwrap()
                .trace_integrity(),
            None
        );

        let retention_offset =
            INVENTORY_MAGIC.len() + 1 + COMPLIANCE_KEY_ID_LEN + POLICY_ENCODED_LEN + 8;
        let mut pre_fix_v2 = canonical_v2;
        pre_fix_v2[retention_offset..retention_offset + 8].copy_from_slice(&DAY_MS.to_be_bytes());
        assert!(matches!(
            DestructionInventory::decode(&pre_fix_v2),
            Err(ComplianceError::IncompleteInventory)
        ));
    }

    #[test]
    fn trace_integrity_evidence_is_durable_and_monotonic() {
        let commitment = [61; 32];
        let verified =
            TraceIntegrityEvidence::new(TraceIntegrityStatus::Verified, commitment).unwrap();
        let degraded =
            TraceIntegrityEvidence::new(TraceIntegrityStatus::Degraded, commitment).unwrap();
        let mut inventory = inventory();

        inventory.record_trace_integrity(verified).unwrap();
        assert_eq!(inventory.trace_integrity(), Some(verified));
        inventory.record_trace_integrity(degraded).unwrap();
        assert_eq!(inventory.trace_integrity(), Some(degraded));
        inventory.record_trace_integrity(verified).unwrap();
        assert_eq!(inventory.trace_integrity(), Some(degraded));
        assert_eq!(
            inventory.record_trace_integrity(
                TraceIntegrityEvidence::new(TraceIntegrityStatus::Degraded, [62; 32]).unwrap()
            ),
            Err(ComplianceError::StateConflict)
        );

        let encoded = inventory.encode().unwrap();
        assert_eq!(
            DestructionInventory::decode(&encoded)
                .unwrap()
                .trace_integrity(),
            Some(degraded)
        );
        let state_offset =
            INVENTORY_MAGIC.len() + 1 + COMPLIANCE_KEY_ID_LEN + POLICY_ENCODED_LEN + 8 + 8;
        let trace_integrity_offset = state_offset + 1;
        let mut zero_commitment = encoded;
        zero_commitment[trace_integrity_offset + 1..trace_integrity_offset + 33].fill(0);
        assert!(matches!(
            DestructionInventory::decode(&zero_commitment),
            Err(ComplianceError::IncompleteInventory)
        ));
    }

    #[test]
    fn retained_inventory_updates_are_strictly_monotonic() {
        let mut inventory = inventory();
        let original = inventory.copies().to_vec();
        assert_eq!(
            inventory.update_copies_monotonic(original.clone()),
            Err(ComplianceError::StateConflict)
        );

        let mut extended = original.clone();
        extended.push(KeyCopy::present(KeyCopyKind::Backup, b"second-backup").unwrap());
        assert_eq!(inventory.update_copies_monotonic(extended), Ok(1));

        let mut removed = inventory.copies().to_vec();
        removed.remove(0);
        assert_eq!(
            inventory.update_copies_monotonic(removed),
            Err(ComplianceError::StateConflict)
        );

        let mut mutated = inventory.copies().to_vec();
        let first_kind = mutated[0].kind();
        mutated[0] = KeyCopy::present(first_kind, b"different-copy").unwrap();
        mutated.push(KeyCopy::present(KeyCopyKind::Backup, b"third-backup").unwrap());
        assert_eq!(
            inventory.update_copies_monotonic(mutated),
            Err(ComplianceError::StateConflict)
        );
    }

    #[test]
    fn counsel_policy_update_can_extend_but_never_shorten_tr_retention() {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Tr,
            [5; 32],
            0,
            1,
        );
        let mut inventory = DestructionInventory::new(key_id, 0, policy(), all_copies()).unwrap();
        let copies = inventory.copies().to_vec();
        assert_eq!(
            inventory.update_policy_and_copies_monotonic(
                RetentionPolicy::new(500, [43; 32]).unwrap(),
                copies.clone(),
            ),
            Ok((true, 0))
        );
        assert_eq!(inventory.retention_policy().tr_days(), 500);
        assert_eq!(inventory.retention_until_ms(), 501 * DAY_MS);

        assert_eq!(
            inventory.update_policy_and_copies_monotonic(
                RetentionPolicy::new(400, [44; 32]).unwrap(),
                copies.clone(),
            ),
            Err(ComplianceError::StateConflict)
        );
        assert_eq!(
            inventory.update_policy_and_copies_monotonic(
                RetentionPolicy::new(600, [43; 32]).unwrap(),
                copies,
            ),
            Err(ComplianceError::StateConflict)
        );
    }

    #[test]
    fn inventory_codec_rejects_unknown_or_inconsistent_state() {
        let mut encoded = inventory().encode().unwrap();
        let state_offset =
            INVENTORY_MAGIC.len() + 1 + COMPLIANCE_KEY_ID_LEN + POLICY_ENCODED_LEN + 8 + 8;
        encoded[state_offset] = 99;
        assert!(matches!(
            DestructionInventory::decode(&encoded),
            Err(ComplianceError::IncompleteInventory)
        ));

        let mut encoded = inventory().encode().unwrap();
        let first_copy_state = state_offset + 1 + 1 + 1 + 32 + 1;
        encoded[first_copy_state] = copy_state_byte(CopyState::Destroyed);
        assert!(matches!(
            DestructionInventory::decode(&encoded),
            Err(ComplianceError::IncompleteInventory)
        ));
    }
}
