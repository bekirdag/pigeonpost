//! Separately encrypted private disclosure audit records.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::approval::{commit, AuthorizedDisclosure, CommitmentOpenings, DisclosureRequest};
use crate::error::{ComplianceError, Result};
use crate::ledger::DisclosureIntent;

const AUDIT_MAGIC: &[u8; 8] = b"PPAUDIT\0";
const LEGACY_AUDIT_VERSION: u8 = 2;
const AUDIT_VERSION: u8 = 3;
const AUDIT_AAD_DOMAIN: &[u8] = b"pigeonpost/private-disclosure-audit/v1";
const RESULT_COMMITMENT_DOMAIN: &[u8] = b"pigeonpost/disclosure-output/v3";
const MAX_PRIVATE_FIELD_BYTES: usize = 4 * 1024;
const MAX_PRIVATE_PLAINTEXT_BYTES: usize = 16 * 1024;
// Version + request id + three non-empty private fields + their three 32-byte salts + approver
// count + two (non-empty identity, public key, commitment salt) tuples.
const MIN_LEGACY_PRIVATE_PLAINTEXT_BYTES: usize = 1 + 32 + 3 * 3 + 3 * 32 + 1 + 2 * (3 + 32 + 32);
// Version 3 adds a presence tag plus an exact 32-byte terminal-manifest commitment slot.
const MIN_PRIVATE_PLAINTEXT_BYTES: usize = MIN_LEGACY_PRIVATE_PLAINTEXT_BYTES + 1 + 32;
const AUDIT_FIXED_LEN: usize = 8 + 1 + 32 + 24 + 4;

/// Dedicated key for the private case record, distinct from every attribution/trace custody key.
#[derive(ZeroizeOnDrop)]
pub struct PrivateAuditKey {
    secret: [u8; 32],
}

impl PrivateAuditKey {
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        while secret == [0u8; 32] {
            OsRng.fill_bytes(&mut secret);
        }
        Self { secret }
    }

    pub fn from_bytes(secret: [u8; 32]) -> Result<Self> {
        if secret == [0u8; 32] {
            return Err(ComplianceError::InvalidKey);
        }
        Ok(Self { secret })
    }

    /// A non-dictionary-testable commitment to the exact disclosed artifact. Verification is
    /// intentionally restricted to the private-audit custodian that already holds this key.
    pub fn result_commitment(&self, request_id: &[u8; 32], output: &[u8]) -> [u8; 32] {
        self.commit_result(request_id, None, output)
    }

    /// Commit a trace disclosure to both its bytes and its authenticated terminal manifest.
    pub fn result_commitment_with_terminal_manifest(
        &self,
        request_id: &[u8; 32],
        terminal_manifest_commitment: &[u8; 32],
        output: &[u8],
    ) -> Result<[u8; 32]> {
        if *terminal_manifest_commitment == [0u8; 32] {
            return Err(ComplianceError::InvalidRequest);
        }
        Ok(self.commit_result(request_id, Some(terminal_manifest_commitment), output))
    }

    fn commit_result(
        &self,
        request_id: &[u8; 32],
        terminal_manifest_commitment: Option<&[u8; 32]>,
        output: &[u8],
    ) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(RESULT_COMMITMENT_DOMAIN);
        hash.update(self.secret);
        hash.update(request_id);
        match terminal_manifest_commitment {
            Some(commitment) => {
                hash.update([1]);
                hash.update(commitment);
            }
            None => {
                hash.update([0]);
                hash.update([0u8; 32]);
            }
        }
        hash.update((output.len() as u64).to_be_bytes());
        hash.update(output);
        hash.finalize().into()
    }
}

impl core::fmt::Debug for PrivateAuditKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PrivateAuditKey(<withheld>)")
    }
}

/// Borrowed private case values. They are encoded directly into zeroizing plaintext storage.
#[derive(Clone, Copy)]
pub struct PrivateAuditMaterial<'a> {
    pub order_reference: &'a [u8],
    pub requester_identity: &'a [u8],
    pub selectors: &'a [u8],
    pub approver_identities: [&'a [u8]; 2],
}

impl core::fmt::Debug for PrivateAuditMaterial<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PrivateAuditMaterial(<withheld>)")
    }
}

/// Persistable encrypted record. Its debug form reveals no nonce, ciphertext, or case value.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedPrivateAuditRecord {
    version: u8,
    request_id: [u8; 32],
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

impl EncryptedPrivateAuditRecord {
    pub const fn request_id(&self) -> [u8; 32] {
        self.request_id
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let minimum = minimum_plaintext_bytes(self.version)?;
        if !(minimum + 16..=MAX_PRIVATE_PLAINTEXT_BYTES + 16).contains(&self.ciphertext.len()) {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut out = Vec::with_capacity(AUDIT_FIXED_LEN + self.ciphertext.len());
        out.extend_from_slice(AUDIT_MAGIC);
        out.push(self.version);
        out.extend_from_slice(&self.request_id);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&(self.ciphertext.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.ciphertext);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < AUDIT_FIXED_LEN + MIN_LEGACY_PRIVATE_PLAINTEXT_BYTES + 16
            || &bytes[..8] != AUDIT_MAGIC
        {
            return Err(ComplianceError::InvalidRequest);
        }
        let version = bytes[8];
        let minimum = minimum_plaintext_bytes(version)?;
        let length = u32::from_be_bytes(bytes[65..69].try_into().expect("fixed slice")) as usize;
        if !(minimum + 16..=MAX_PRIVATE_PLAINTEXT_BYTES + 16).contains(&length)
            || bytes.len() != AUDIT_FIXED_LEN + length
        {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut request_id = [0u8; 32];
        request_id.copy_from_slice(&bytes[9..41]);
        let mut nonce = [0u8; 24];
        nonce.copy_from_slice(&bytes[41..65]);
        Ok(Self {
            version,
            request_id,
            nonce,
            ciphertext: bytes[69..].to_vec(),
        })
    }
}

impl core::fmt::Debug for EncryptedPrivateAuditRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EncryptedPrivateAuditRecord")
            .field("request_id", &self.request_id)
            .field(
                "encrypted_payload",
                &format_args!("<{} bytes>", self.ciphertext.len()),
            )
            .finish()
    }
}

/// Decrypted private record. All owned raw values are zeroized on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateAuditRecord {
    pub request_id: [u8; 32],
    terminal_manifest_commitment: Option<[u8; 32]>,
    pub order_reference: Vec<u8>,
    pub requester_identity: Vec<u8>,
    pub selectors: Vec<u8>,
    pub approver_identities: [Vec<u8>; 2],
    field_salts: [[u8; 32]; 3],
    approver_public_keys: [[u8; 32]; 2],
    approver_salts: [[u8; 32]; 2],
}

impl PrivateAuditRecord {
    /// Exact signed terminal-manifest commitment for trace disclosures. Attribution records have
    /// no terminal manifest and legacy version-2 records decode as `None`.
    pub const fn terminal_manifest_commitment(&self) -> Option<[u8; 32]> {
        self.terminal_manifest_commitment
    }

    /// Prove that this encrypted case record opens every private commitment in a public intent.
    /// The approver identities remain custodian assertions bound by the pinned key roster; the
    /// public keys and salts prove which two roster keys authorized the exact request.
    pub fn verifies_intent(&self, intent: &DisclosureIntent) -> bool {
        self.request_id == intent.request_id
            && commit(b"order", &self.field_salts[0], &self.order_reference)
                == intent.order_commitment
            && commit(b"requester", &self.field_salts[1], &self.requester_identity)
                == intent.requester_commitment
            && commit(b"selectors", &self.field_salts[2], &self.selectors)
                == intent.selector_commitment
            && intent.approver_commitments.len() == 2
            && self
                .approver_public_keys
                .iter()
                .zip(self.approver_salts.iter())
                .zip(intent.approver_commitments.iter())
                .all(|((key, salt), expected)| commit(b"approver", salt, key) == *expected)
    }
}

impl core::fmt::Debug for PrivateAuditRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PrivateAuditRecord")
            .field("request_id", &self.request_id)
            .field("private_case_fields", &"<withheld>")
            .finish()
    }
}

impl Drop for PrivateAuditRecord {
    fn drop(&mut self) {
        self.order_reference.zeroize();
        self.requester_identity.zeroize();
        self.selectors.zeroize();
        self.approver_identities[0].zeroize();
        self.approver_identities[1].zeroize();
        self.field_salts.zeroize();
        self.approver_public_keys.zeroize();
        self.approver_salts.zeroize();
    }
}

/// Encrypt the raw private record only after proving it opens the request's public commitments.
pub fn seal_private_audit_record(
    key: &PrivateAuditKey,
    request: &DisclosureRequest,
    openings: &CommitmentOpenings,
    material: PrivateAuditMaterial<'_>,
) -> Result<EncryptedPrivateAuditRecord> {
    seal_private_audit_record_inner(key, request, openings, material, None)
}

/// Encrypt a trace case record while durably binding it to the exact authenticated terminal
/// manifest consumed by the operation.
pub fn seal_private_audit_record_with_terminal_manifest(
    key: &PrivateAuditKey,
    request: &DisclosureRequest,
    openings: &CommitmentOpenings,
    material: PrivateAuditMaterial<'_>,
    terminal_manifest_commitment: [u8; 32],
) -> Result<EncryptedPrivateAuditRecord> {
    if terminal_manifest_commitment == [0u8; 32] {
        return Err(ComplianceError::InvalidRequest);
    }
    seal_private_audit_record_inner(
        key,
        request,
        openings,
        material,
        Some(terminal_manifest_commitment),
    )
}

fn seal_private_audit_record_inner(
    key: &PrivateAuditKey,
    request: &DisclosureRequest,
    openings: &CommitmentOpenings,
    material: PrivateAuditMaterial<'_>,
    terminal_manifest_commitment: Option<[u8; 32]>,
) -> Result<EncryptedPrivateAuditRecord> {
    if request.approver_commitments().len() != 2
        || !openings.verifies_order(request, material.order_reference)
        || !openings.verifies_requester(request, material.requester_identity)
        || !openings.verifies_selectors(request, material.selectors)
    {
        return Err(ComplianceError::InvalidRequest);
    }
    let (approver_keys, approver_salts) = request.approver_openings();
    if approver_keys.len() != 2
        || approver_salts.len() != 2
        || approver_keys
            .iter()
            .zip(approver_salts.iter())
            .zip(request.approver_commitments().iter())
            .any(|((key, salt), expected)| commit(b"approver", salt, key) != *expected)
    {
        return Err(ComplianceError::InvalidRequest);
    }
    let fields = [
        material.order_reference,
        material.requester_identity,
        material.selectors,
        material.approver_identities[0],
        material.approver_identities[1],
    ];
    if fields
        .iter()
        .any(|field| field.is_empty() || field.len() > MAX_PRIVATE_FIELD_BYTES)
    {
        return Err(ComplianceError::LimitExceeded);
    }
    let fixed_without_payloads: usize = 1 + 32 + 1 + 32 + 2 * 3 + 3 * 32 + 1 + 2 * (2 + 32 + 32);
    let size = fixed_without_payloads
        .checked_add(fields.iter().map(|field| field.len()).sum::<usize>())
        .ok_or(ComplianceError::LimitExceeded)?;
    if size > MAX_PRIVATE_PLAINTEXT_BYTES {
        return Err(ComplianceError::LimitExceeded);
    }
    let mut plaintext = Zeroizing::new(Vec::with_capacity(size));
    plaintext.push(AUDIT_VERSION);
    plaintext.extend_from_slice(&request.request_id());
    match terminal_manifest_commitment {
        Some(commitment) => {
            plaintext.push(1);
            plaintext.extend_from_slice(&commitment);
        }
        None => {
            plaintext.push(0);
            plaintext.extend_from_slice(&[0u8; 32]);
        }
    }
    for field in &fields[..3] {
        push_field(&mut plaintext, field)?;
    }
    for salt in openings.salts() {
        plaintext.extend_from_slice(&salt);
    }
    plaintext.push(2);
    for ((identity, key), salt) in fields[3..]
        .iter()
        .zip(approver_keys.iter())
        .zip(approver_salts.iter())
    {
        push_field(&mut plaintext, identity)?;
        plaintext.extend_from_slice(key);
        plaintext.extend_from_slice(salt);
    }
    let request_id = request.request_id();
    let aad = audit_aad(AUDIT_VERSION, &request_id)?;
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = XChaCha20Poly1305::new((&key.secret).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| ComplianceError::Crypto)?;
    Ok(EncryptedPrivateAuditRecord {
        version: AUDIT_VERSION,
        request_id,
        nonce,
        ciphertext,
    })
}

/// Open the private record only inside the same authorized disclosure operation.
pub fn open_private_audit_record(
    authorization: &AuthorizedDisclosure,
    key: &PrivateAuditKey,
    encrypted: &EncryptedPrivateAuditRecord,
) -> Result<PrivateAuditRecord> {
    if authorization.request_id() != encrypted.request_id {
        return Err(ComplianceError::Unauthorized);
    }
    let aad = audit_aad(encrypted.version, &encrypted.request_id)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new((&key.secret).into())
            .decrypt(
                XNonce::from_slice(&encrypted.nonce),
                Payload {
                    msg: &encrypted.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| ComplianceError::Crypto)?,
    );
    decode_plaintext(&plaintext, encrypted.version)
}

fn decode_plaintext(bytes: &[u8], expected_version: u8) -> Result<PrivateAuditRecord> {
    if bytes.is_empty() || bytes.len() > MAX_PRIVATE_PLAINTEXT_BYTES {
        return Err(ComplianceError::InvalidRequest);
    }
    let version = bytes[0];
    if version != expected_version || bytes.len() < minimum_plaintext_bytes(version)? {
        return Err(ComplianceError::InvalidRequest);
    }
    let mut cursor = 1;
    let mut request_id = [0u8; 32];
    request_id.copy_from_slice(&bytes[cursor..cursor + 32]);
    cursor += 32;
    let terminal_manifest_commitment = if version == AUDIT_VERSION {
        if bytes.len().saturating_sub(cursor) < 33 {
            return Err(ComplianceError::InvalidRequest);
        }
        let present = bytes[cursor];
        cursor += 1;
        let commitment = read_array32(bytes, &mut cursor)?;
        match present {
            0 if commitment == [0u8; 32] => None,
            1 if commitment != [0u8; 32] => Some(commitment),
            _ => return Err(ComplianceError::InvalidRequest),
        }
    } else {
        None
    };
    let order_reference = read_field(bytes, &mut cursor)?;
    let requester_identity = read_field(bytes, &mut cursor)?;
    let selectors = read_field(bytes, &mut cursor)?;
    let field_salts = [
        read_array32(bytes, &mut cursor)?,
        read_array32(bytes, &mut cursor)?,
        read_array32(bytes, &mut cursor)?,
    ];
    if cursor >= bytes.len() || bytes[cursor] != 2 {
        return Err(ComplianceError::InvalidRequest);
    }
    cursor += 1;
    let first = read_field(bytes, &mut cursor)?;
    let first_key = read_array32(bytes, &mut cursor)?;
    let first_salt = read_array32(bytes, &mut cursor)?;
    let second = read_field(bytes, &mut cursor)?;
    let second_key = read_array32(bytes, &mut cursor)?;
    let second_salt = read_array32(bytes, &mut cursor)?;
    if cursor != bytes.len() {
        return Err(ComplianceError::InvalidRequest);
    }
    Ok(PrivateAuditRecord {
        request_id,
        terminal_manifest_commitment,
        order_reference,
        requester_identity,
        selectors,
        approver_identities: [first, second],
        field_salts,
        approver_public_keys: [first_key, second_key],
        approver_salts: [first_salt, second_salt],
    })
}

fn push_field(out: &mut Vec<u8>, field: &[u8]) -> Result<()> {
    let length: u16 = field
        .len()
        .try_into()
        .map_err(|_| ComplianceError::LimitExceeded)?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(field);
    Ok(())
}

fn read_field(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    if bytes.len().saturating_sub(*cursor) < 2 {
        return Err(ComplianceError::InvalidRequest);
    }
    let length =
        u16::from_be_bytes(bytes[*cursor..*cursor + 2].try_into().expect("fixed slice")) as usize;
    *cursor += 2;
    if length == 0
        || length > MAX_PRIVATE_FIELD_BYTES
        || bytes.len().saturating_sub(*cursor) < length
    {
        return Err(ComplianceError::InvalidRequest);
    }
    let value = bytes[*cursor..*cursor + length].to_vec();
    *cursor += length;
    Ok(value)
}

fn read_array32(bytes: &[u8], cursor: &mut usize) -> Result<[u8; 32]> {
    if bytes.len().saturating_sub(*cursor) < 32 {
        return Err(ComplianceError::InvalidRequest);
    }
    let mut value = [0u8; 32];
    value.copy_from_slice(&bytes[*cursor..*cursor + 32]);
    *cursor += 32;
    Ok(value)
}

fn audit_aad(version: u8, request_id: &[u8; 32]) -> Result<Vec<u8>> {
    minimum_plaintext_bytes(version)?;
    let mut aad = Vec::with_capacity(AUDIT_AAD_DOMAIN.len() + 1 + 32);
    aad.extend_from_slice(AUDIT_AAD_DOMAIN);
    aad.push(version);
    aad.extend_from_slice(request_id);
    Ok(aad)
}

fn minimum_plaintext_bytes(version: u8) -> Result<usize> {
    match version {
        LEGACY_AUDIT_VERSION => Ok(MIN_LEGACY_PRIVATE_PLAINTEXT_BYTES),
        AUDIT_VERSION => Ok(MIN_PRIVATE_PLAINTEXT_BYTES),
        _ => Err(ComplianceError::InvalidRequest),
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};
    use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};

    use super::*;
    use crate::approval::{DisclosureRequest, SensitiveRequestMaterial};
    use crate::ledger::{DisclosureLeaf, DisclosureLedger, DisclosureOutput};

    #[test]
    fn private_values_are_encrypted_strict_and_open_only_under_authorization() {
        let material = SensitiveRequestMaterial {
            order_reference: b"order-secret-123",
            requester_identity: b"requester-secret",
            selectors: b"203.0.113.5 person@example.invalid",
        };
        let (mut request, openings) = DisclosureRequest::new(
            Jurisdiction::Test,
            CompliancePurpose::Attribution,
            vec![ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [1; 32],
                1,
                1,
            )],
            100,
            300,
            material,
        )
        .unwrap();
        for seed in [[2u8; 32], [3u8; 32]] {
            let signer = SigningKey::from_bytes(&seed);
            let signature = signer.sign(&request.approval_preimage(150)).to_bytes();
            request
                .approve(signer.verifying_key().to_bytes(), 150, signature)
                .unwrap();
        }
        let key = PrivateAuditKey::from_bytes([9; 32]).unwrap();
        let encrypted = seal_private_audit_record(
            &key,
            &request,
            &openings,
            PrivateAuditMaterial {
                order_reference: material.order_reference,
                requester_identity: material.requester_identity,
                selectors: material.selectors,
                approver_identities: [b"officer one", b"outside counsel"],
            },
        )
        .unwrap();
        let encoded = encrypted.encode().unwrap();
        assert_eq!(
            EncryptedPrivateAuditRecord::decode(&encoded).unwrap(),
            encrypted
        );
        let text = String::from_utf8_lossy(&encoded);
        assert!(!text.contains("order-secret-123"));
        assert!(!text.contains("requester-secret"));
        assert!(!text.contains("203.0.113.5"));
        assert!(!format!("{encrypted:?}").contains("order-secret-123"));

        let terminal_manifest_commitment = [42u8; 32];
        let trace_encrypted = seal_private_audit_record_with_terminal_manifest(
            &key,
            &request,
            &openings,
            PrivateAuditMaterial {
                order_reference: material.order_reference,
                requester_identity: material.requester_identity,
                selectors: material.selectors,
                approver_identities: [b"officer one", b"outside counsel"],
            },
            terminal_manifest_commitment,
        )
        .unwrap();
        assert!(seal_private_audit_record_with_terminal_manifest(
            &key,
            &request,
            &openings,
            PrivateAuditMaterial {
                order_reference: material.order_reference,
                requester_identity: material.requester_identity,
                selectors: material.selectors,
                approver_identities: [b"officer one", b"outside counsel"],
            },
            [0u8; 32],
        )
        .is_err());
        let plain_output = b"selected trace records";
        let unbound = key.result_commitment(&request.request_id(), plain_output);
        let bound = key
            .result_commitment_with_terminal_manifest(
                &request.request_id(),
                &terminal_manifest_commitment,
                plain_output,
            )
            .unwrap();
        assert_ne!(unbound, bound);
        let mut other_manifest = terminal_manifest_commitment;
        other_manifest[0] ^= 1;
        assert_ne!(
            bound,
            key.result_commitment_with_terminal_manifest(
                &request.request_id(),
                &other_manifest,
                plain_output,
            )
            .unwrap()
        );

        let mut ledger = DisclosureLedger::in_memory();
        let opened = ledger
            .execute(
                &mut request,
                160,
                || Ok(170),
                |authorization| {
                    let value = open_private_audit_record(authorization, &key, &encrypted)?;
                    let trace = open_private_audit_record(authorization, &key, &trace_encrypted)?;
                    assert_eq!(
                        trace.terminal_manifest_commitment(),
                        Some(terminal_manifest_commitment)
                    );
                    Ok(DisclosureOutput {
                        value,
                        record_count: 1,
                        result_commitment: [7; 32],
                    })
                },
            )
            .unwrap();
        assert_eq!(opened.order_reference, material.order_reference);
        assert_eq!(opened.terminal_manifest_commitment(), None);
        let DisclosureLeaf::Intent(intent) = ledger.leaf(0).unwrap().unwrap() else {
            panic!("the first disclosure leaf must be its intent");
        };
        assert!(opened.verifies_intent(&intent));

        let mut different_intent = intent.clone();
        different_intent.selector_commitment[0] ^= 1;
        assert!(!opened.verifies_intent(&different_intent));

        let mut extended = encoded;
        extended.push(0);
        assert!(EncryptedPrivateAuditRecord::decode(&extended).is_err());
    }
}
