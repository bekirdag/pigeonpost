//! Offline-only attribution unsealing and trace epoch-key custody.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::Signature;
use hkdf::Hkdf;
use pigeonpost_compliance_format::{
    attribution_aad, attribution_epoch_contains, attribution_epoch_end_ms, attribution_hkdf_salt,
    attribution_signing_preimage, AttributionClaim, ComplianceKeyId, CompliancePurpose,
    Jurisdiction, ATTRIBUTION_BLOCK_VERSION, ATTRIBUTION_HKDF_INFO,
};
use pigeonpost_compliance_seal::{
    trace_key_wrap_salt, IdentityTraceRecord, TraceRecord, VerifiedSegment, WrappedEpochKey,
    TRACE_KEY_WRAP_HKDF_INFO,
};
use pigeonpost_core::envelope::{
    AttributionBlock, MAX_ATTRIBUTION_CLOCK_SKEW_MS, MAX_TIMESTAMP_JITTER_SECS,
};
use pigeonpost_core::keys;
use pigeonpost_core::Wrap;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::approval::AuthorizedDisclosure;
use crate::error::{ComplianceError, Result};

#[derive(ZeroizeOnDrop)]
struct OfflineEpochKey {
    #[zeroize(skip)]
    key_id: ComplianceKeyId,
    secret: [u8; 32],
}

/// Boundary implemented by a production HSM/KMS adapter or threshold-custody coordinator.
/// Implementations must bind each instance to one published purpose and jurisdiction.
pub trait CustodyBackend: core::fmt::Debug {
    fn purpose(&self) -> CompliancePurpose;
    fn jurisdiction(&self) -> Jurisdiction;
    fn public_key(&self) -> [u8; 32];
    fn agree(&self, peer_public_key: &[u8; 32]) -> Result<[u8; 32]>;
}

/// Zeroizing software backend for offline development and recovery ceremonies.
/// Production deployments should implement [`CustodyBackend`] over their configured custodian.
#[derive(ZeroizeOnDrop)]
pub struct SoftwareCustodyKey {
    #[zeroize(skip)]
    purpose: CompliancePurpose,
    #[zeroize(skip)]
    jurisdiction: Jurisdiction,
    secret: [u8; 32],
}

impl SoftwareCustodyKey {
    pub fn generate(purpose: CompliancePurpose, jurisdiction: Jurisdiction) -> Result<Self> {
        if jurisdiction != Jurisdiction::Test {
            return Err(ComplianceError::InvalidKey);
        }
        let mut secret = [0u8; 32];
        while secret == [0u8; 32] {
            OsRng.fill_bytes(&mut secret);
        }
        Ok(Self {
            purpose,
            jurisdiction,
            secret,
        })
    }

    pub fn from_bytes(
        purpose: CompliancePurpose,
        jurisdiction: Jurisdiction,
        secret: [u8; 32],
    ) -> Result<Self> {
        if jurisdiction != Jurisdiction::Test || secret == [0u8; 32] {
            return Err(ComplianceError::InvalidKey);
        }
        Ok(Self {
            purpose,
            jurisdiction,
            secret,
        })
    }
}

impl CustodyBackend for SoftwareCustodyKey {
    fn purpose(&self) -> CompliancePurpose {
        self.purpose
    }

    fn jurisdiction(&self) -> Jurisdiction {
        self.jurisdiction
    }

    fn public_key(&self) -> [u8; 32] {
        MontgomeryPoint::mul_base_clamped(self.secret).to_bytes()
    }

    fn agree(&self, peer_public_key: &[u8; 32]) -> Result<[u8; 32]> {
        let shared = MontgomeryPoint(*peer_public_key)
            .mul_clamped(self.secret)
            .to_bytes();
        if shared == [0u8; 32] {
            return Err(ComplianceError::InvalidKey);
        }
        Ok(shared)
    }
}

impl core::fmt::Debug for SoftwareCustodyKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SoftwareCustodyKey(<withheld>)")
    }
}

/// Verified sender claim returned only inside an authorized offline disclosure.
#[derive(Clone, PartialEq, Eq)]
pub struct AttributionDisclosure {
    pub key_id: ComplianceKeyId,
    pub event_id: [u8; 32],
    pub recipient: [u8; 32],
    pub sender_public_key: [u8; 32],
    pub sent_at_ms: u64,
}

impl core::fmt::Debug for AttributionDisclosure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The event id is a raw disclosure selector and the timestamp/key association narrows it
        // further. Ordinary instrumentation must reveal no part of an authorized disclosure.
        f.write_str("AttributionDisclosure(<withheld>)")
    }
}

impl Drop for AttributionDisclosure {
    fn drop(&mut self) {
        self.event_id.zeroize();
        self.recipient.zeroize();
        self.sender_public_key.zeroize();
        self.sent_at_ms = 0;
    }
}

/// Unseal and authenticate a v3 attribution claim. Legacy blocks and mismatched registry keys
/// fail closed; the envelope's content seal is neither needed nor opened.
pub fn unseal_attribution(
    authorization: &AuthorizedDisclosure,
    custody_key_id: &ComplianceKeyId,
    custody: &impl CustodyBackend,
    wrapped: &Wrap,
) -> Result<AttributionDisclosure> {
    wrapped
        .verify_public()
        .map_err(|_| ComplianceError::AttributionInvalid)?;
    let Some(AttributionBlock::V3(block)) = wrapped.attribution.as_ref() else {
        return Err(ComplianceError::AttributionInvalid);
    };
    if custody_key_id.purpose != CompliancePurpose::Attribution
        || custody.purpose() != CompliancePurpose::Attribution
        || custody.jurisdiction() != custody_key_id.jurisdiction
        || block.block_version != ATTRIBUTION_BLOCK_VERSION
        || block.key_id != *custody_key_id
        || !authorization.permits(custody_key_id)
    {
        return Err(ComplianceError::WrongPurpose);
    }
    if attribution_epoch_end_ms(custody_key_id).is_err() {
        return Err(ComplianceError::AttributionInvalid);
    }
    let custody_public = custody.public_key();
    let expected_digest: [u8; 32] = Sha256::digest(custody_public).into();
    if !bool::from(block.compliance_key_digest.ct_eq(&expected_digest)) {
        return Err(ComplianceError::InvalidKey);
    }
    let shared = Zeroizing::new(custody.agree(&block.e_pk)?);
    let salt = Zeroizing::new(attribution_hkdf_salt(
        &block.e_pk,
        &block.compliance_key_digest,
    ));
    let mut aead_key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&salt[..]), &shared[..])
        .expand(ATTRIBUTION_HKDF_INFO, &mut *aead_key)
        .map_err(|_| ComplianceError::Crypto)?;
    let event_id = wrapped.id();
    let aad = attribution_aad(
        block.block_version,
        &block.key_id,
        &block.compliance_key_digest,
        &block.e_pk,
        &event_id,
        &wrapped.recipient,
    )
    .map_err(|_| ComplianceError::AttributionInvalid)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new((&*aead_key).into())
            .decrypt(
                XNonce::from_slice(&block.nonce),
                Payload {
                    msg: &block.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| ComplianceError::AttributionInvalid)?,
    );
    let claim =
        AttributionClaim::decode(&plaintext).map_err(|_| ComplianceError::AttributionInvalid)?;
    let sender = keys::verifying_key_from_bytes(&claim.sender_pubkey)
        .map_err(|_| ComplianceError::AttributionInvalid)?;
    let signing_preimage = attribution_signing_preimage(
        block.block_version,
        &block.key_id,
        &block.compliance_key_digest,
        &block.e_pk,
        &event_id,
        &wrapped.recipient,
        claim.sent_at_ms,
    )
    .map_err(|_| ComplianceError::AttributionInvalid)?;
    keys::verify(
        &sender,
        &signing_preimage,
        &Signature::from_bytes(&claim.signature),
    )
    .map_err(|_| ComplianceError::AttributionInvalid)?;
    let visible_ms = wrapped
        .created_at
        .checked_mul(1_000)
        .ok_or(ComplianceError::AttributionInvalid)?;
    let latest = visible_ms
        .saturating_add(MAX_TIMESTAMP_JITTER_SECS.saturating_mul(1_000))
        .saturating_add(MAX_ATTRIBUTION_CLOCK_SKEW_MS);
    if claim.sent_at_ms < visible_ms
        || claim.sent_at_ms > latest
        || attribution_epoch_contains(custody_key_id, claim.sent_at_ms) != Ok(true)
    {
        return Err(ComplianceError::AttributionInvalid);
    }
    Ok(AttributionDisclosure {
        key_id: block.key_id,
        event_id,
        recipient: wrapped.recipient,
        sender_public_key: claim.sender_pubkey,
        sent_at_ms: claim.sent_at_ms,
    })
}

/// Unwrap a daily trace key only after the two-person request's intent was recorded.
fn unwrap_trace_epoch_key(
    authorization: &AuthorizedDisclosure,
    expected_purpose: CompliancePurpose,
    expected_jurisdiction: Jurisdiction,
    custody: &impl CustodyBackend,
    wrapped: &WrappedEpochKey,
) -> Result<OfflineEpochKey> {
    if !matches!(
        expected_purpose,
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace
    ) || wrapped.key_id().purpose != expected_purpose
        || wrapped.key_id().jurisdiction != expected_jurisdiction
        || custody.purpose() != expected_purpose
        || custody.jurisdiction() != expected_jurisdiction
        || !authorization.permits(&wrapped.key_id())
    {
        return Err(ComplianceError::WrongPurpose);
    }
    let custody_public = custody.public_key();
    let expected_digest: [u8; 32] = Sha256::digest(custody_public).into();
    if !bool::from(wrapped.compliance_key_digest().ct_eq(&expected_digest)) {
        return Err(ComplianceError::InvalidKey);
    }
    let shared = Zeroizing::new(custody.agree(&wrapped.ephemeral_public_key())?);
    let salt = Zeroizing::new(trace_key_wrap_salt(
        &wrapped.ephemeral_public_key(),
        &wrapped.compliance_key_digest(),
    ));
    let mut aead_key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&salt[..]), &shared[..])
        .expand(TRACE_KEY_WRAP_HKDF_INFO, &mut *aead_key)
        .map_err(|_| ComplianceError::Crypto)?;
    let aad = wrapped.aad().map_err(|_| ComplianceError::SegmentInvalid)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new((&*aead_key).into())
            .decrypt(
                XNonce::from_slice(&wrapped.nonce()),
                Payload {
                    msg: wrapped.ciphertext(),
                    aad: &aad,
                },
            )
            .map_err(|_| ComplianceError::Crypto)?,
    );
    let mut secret: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| ComplianceError::Crypto)?;
    let commitment: [u8; 32] = Sha256::digest(secret).into();
    if !bool::from(commitment.ct_eq(&wrapped.epoch_key_commitment())) {
        secret.zeroize();
        return Err(ComplianceError::InvalidKey);
    }
    let epoch = OfflineEpochKey {
        key_id: wrapped.key_id(),
        secret,
    };
    secret.zeroize();
    Ok(epoch)
}

/// Decrypted trace record, still deliberately split by purpose.
#[derive(Clone, PartialEq, Eq)]
pub enum DisclosedTraceRecord {
    Network(TraceRecord),
    Identity(IdentityTraceRecord),
}

impl core::fmt::Debug for DisclosedTraceRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Network(_) => f.write_str("DisclosedTraceRecord::Network(<withheld>)"),
            Self::Identity(_) => f.write_str("DisclosedTraceRecord::Identity(<withheld>)"),
        }
    }
}

/// Decrypt every frame in an already publicly verified segment.
fn decrypt_trace_segment_selected(
    authorization: &AuthorizedDisclosure,
    epoch_key: &OfflineEpochKey,
    segment: &VerifiedSegment,
    expected_node_id: [u8; 32],
    max_selected: usize,
    mut select: impl FnMut(&DisclosedTraceRecord) -> bool,
) -> Result<Vec<DisclosedTraceRecord>> {
    let key_id = segment.header.key_id();
    if key_id != epoch_key.key_id
        || expected_node_id == [0u8; 32]
        || !authorization.permits(&key_id)
    {
        return Err(ComplianceError::WrongPurpose);
    }
    let epoch_commitment: [u8; 32] = Sha256::digest(epoch_key.secret).into();
    if segment.header.wrapped_epoch_key().epoch_key_commitment() != epoch_commitment {
        return Err(ComplianceError::InvalidKey);
    }
    let cipher = XChaCha20Poly1305::new((&epoch_key.secret).into());
    let mut previous_hash = segment.header.hash();
    let mut records = Vec::with_capacity(segment.frames.len().min(max_selected));
    for frame in &segment.frames {
        let aad = frame
            .aad(&segment.header, &previous_hash)
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&frame.nonce()),
                    Payload {
                        msg: frame.ciphertext(),
                        aad: &aad,
                    },
                )
                .map_err(|_| ComplianceError::Crypto)?,
        );
        let record = match key_id.purpose {
            CompliancePurpose::NetworkTrace => TraceRecord::decode(&plaintext)
                .map(DisclosedTraceRecord::Network)
                .map_err(|_| ComplianceError::SegmentInvalid),
            CompliancePurpose::IdentityTrace => IdentityTraceRecord::decode(&plaintext)
                .map(DisclosedTraceRecord::Identity)
                .map_err(|_| ComplianceError::SegmentInvalid),
            CompliancePurpose::Attribution => Err(ComplianceError::WrongPurpose),
        };
        let record = record?;
        validate_decrypted_trace_identity(&record, key_id.jurisdiction, expected_node_id)?;
        if select(&record) {
            if records.len() >= max_selected {
                return Err(ComplianceError::LimitExceeded);
            }
            records.push(record);
        }
        previous_hash = frame.chain_hash();
    }
    Ok(records)
}

fn validate_decrypted_trace_identity(
    record: &DisclosedTraceRecord,
    expected_jurisdiction: Jurisdiction,
    expected_node_id: [u8; 32],
) -> Result<()> {
    let (jurisdiction, node_id) = match record {
        DisclosedTraceRecord::Network(record) => (record.jurisdiction, record.node_id),
        DisclosedTraceRecord::Identity(record) => (record.jurisdiction, record.node_id),
    };
    if expected_node_id == [0u8; 32]
        || jurisdiction != expected_jurisdiction
        || node_id != expected_node_id
    {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(())
}

/// Unwrap and immediately consume an epoch key inside the authorized operation. The plaintext key
/// is never returned to the caller and is zeroized when this function returns.
pub fn disclose_trace_segment(
    authorization: &AuthorizedDisclosure,
    expected_purpose: CompliancePurpose,
    expected_jurisdiction: Jurisdiction,
    custody: &impl CustodyBackend,
    segment: &VerifiedSegment,
    expected_node_id: [u8; 32],
) -> Result<Vec<DisclosedTraceRecord>> {
    let epoch_key = unwrap_trace_epoch_key(
        authorization,
        expected_purpose,
        expected_jurisdiction,
        custody,
        segment.header.wrapped_epoch_key(),
    )?;
    decrypt_trace_segment_selected(
        authorization,
        &epoch_key,
        segment,
        expected_node_id,
        segment.frames.len(),
        |_| true,
    )
}

/// Unwrap one named epoch and retain only records accepted by the offline selector. Every frame is
/// still authenticated in chain order, but rejected plaintext records are dropped immediately
/// instead of accumulating as a standing decrypted epoch copy.
pub fn disclose_trace_segment_selected(
    authorization: &AuthorizedDisclosure,
    expected_purpose: CompliancePurpose,
    expected_jurisdiction: Jurisdiction,
    custody: &impl CustodyBackend,
    segment: &VerifiedSegment,
    expected_node_id: [u8; 32],
    select: impl FnMut(&DisclosedTraceRecord) -> bool,
) -> Result<Vec<DisclosedTraceRecord>> {
    let epoch_key = unwrap_trace_epoch_key(
        authorization,
        expected_purpose,
        expected_jurisdiction,
        custody,
        segment.header.wrapped_epoch_key(),
    )?;
    decrypt_trace_segment_selected(
        authorization,
        &epoch_key,
        segment,
        expected_node_id,
        segment.frames.len(),
        select,
    )
}

/// Operator-only bounded variant. The public selector API preserves its existing all-matches
/// contract; the CLI must enforce a smaller cross-artifact disclosure ceiling before allocating.
#[allow(clippy::too_many_arguments)]
pub(crate) fn disclose_trace_segment_selected_bounded(
    authorization: &AuthorizedDisclosure,
    expected_purpose: CompliancePurpose,
    expected_jurisdiction: Jurisdiction,
    custody: &impl CustodyBackend,
    segment: &VerifiedSegment,
    expected_node_id: [u8; 32],
    max_selected: usize,
    select: impl FnMut(&DisclosedTraceRecord) -> bool,
) -> Result<Vec<DisclosedTraceRecord>> {
    let epoch_key = unwrap_trace_epoch_key(
        authorization,
        expected_purpose,
        expected_jurisdiction,
        custody,
        segment.header.wrapped_epoch_key(),
    )?;
    decrypt_trace_segment_selected(
        authorization,
        &epoch_key,
        segment,
        expected_node_id,
        max_selected,
        select,
    )
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use ed25519_dalek::{Signer, SigningKey};
    use pigeonpost_compliance_seal::{
        verify_segment, EpochSealingKey, NetworkOperation, SegmentWriter, TraceIp,
        TRACE_EPOCH_DURATION_MS,
    };
    use pigeonpost_core::{envelope, Identity};
    use tempfile::tempdir;

    use super::*;
    use crate::approval::{DisclosureRequest, SensitiveRequestMaterial};
    use crate::ledger::{DisclosureLedger, DisclosureOutput};

    fn authorize(key_id: ComplianceKeyId) -> DisclosureRequest {
        let (mut request, _) = DisclosureRequest::new(
            key_id.jurisdiction,
            key_id.purpose,
            vec![key_id],
            100,
            1_000_000,
            SensitiveRequestMaterial {
                order_reference: b"order",
                requester_identity: b"requester",
                selectors: b"selector",
            },
        )
        .unwrap();
        for seed in [[21u8; 32], [22u8; 32]] {
            let signer = SigningKey::from_bytes(&seed);
            let signature = signer.sign(&request.approval_preimage(150)).to_bytes();
            request
                .approve(signer.verifying_key().to_bytes(), 150, signature)
                .unwrap();
        }
        request
    }

    #[test]
    fn custodian_unseals_and_authenticates_attribution() {
        let sender = Identity::from_seed([1; 32]);
        let recipient = Identity::from_seed([2; 32]);
        let custody = SoftwareCustodyKey::from_bytes(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [3; 32],
        )
        .unwrap();
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [4; 32],
            0,
            1,
        );
        let wrapped = envelope::wrap_attributed(
            &sender,
            &recipient.verifying_key(),
            "pigeonpost this",
            200,
            &custody.public_key(),
            &key_id,
        )
        .unwrap();
        let mut request = authorize(key_id);
        let mut ledger = DisclosureLedger::in_memory();
        let disclosure = ledger
            .execute(
                &mut request,
                250,
                || Ok(260),
                |authorization| {
                    let value = unseal_attribution(authorization, &key_id, &custody, &wrapped)?;
                    Ok(DisclosureOutput {
                        value,
                        record_count: 1,
                        result_commitment: [7; 32],
                    })
                },
            )
            .unwrap();
        assert_eq!(
            disclosure.sender_public_key,
            sender.verifying_key().to_bytes()
        );
        assert_eq!(ledger.leaf_count(), 2);
    }

    #[test]
    fn attribution_disclosure_debug_withholds_every_selector_and_association() {
        let disclosure = AttributionDisclosure {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::Attribution,
                Jurisdiction::Test,
                [0xA1; 32],
                1_234_567,
                9,
            ),
            event_id: [0xB2; 32],
            recipient: [0xC3; 32],
            sender_public_key: [0xD4; 32],
            sent_at_ms: 9_876_543_210,
        };

        assert_eq!(
            format!("{disclosure:?}"),
            "AttributionDisclosure(<withheld>)"
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn authorized_trace_unwrap_and_decryption_round_trip() {
        let temp = tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = temp.path().join("trace.ppseg");
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [9; 32],
            TRACE_EPOCH_DURATION_MS,
            1,
        );
        let custody = SoftwareCustodyKey::from_bytes(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [5; 32],
        )
        .unwrap();
        let epoch = EpochSealingKey::from_bytes(key_id, [6; 32]).unwrap();
        let signer = SigningKey::from_bytes(&[7; 32]);
        let mut writer = SegmentWriter::create(
            &path,
            epoch,
            &custody.public_key(),
            signer.verifying_key().to_bytes(),
            TRACE_EPOCH_DURATION_MS + 100,
            2,
        )
        .unwrap();
        writer
            .append_network(&TraceRecord {
                jurisdiction: Jurisdiction::Test,
                operation: NetworkOperation::Publish,
                timestamp_ms: TRACE_EPOCH_DURATION_MS + 101,
                node_id: [1; 32],
                source_ip: TraceIp::V4(Ipv4Addr::new(192, 0, 2, 4)),
                source_port: 9999,
                event_id: Some([2; 32]),
                recipient: Some([3; 32]),
                owner: None,
                size_bytes: 4,
                correlation_id: None,
            })
            .unwrap();
        writer
            .finalize(TRACE_EPOCH_DURATION_MS + 200, &signer)
            .unwrap();
        let segment = verify_segment(&path).unwrap();
        let mut request = authorize(key_id);
        let mut ledger = DisclosureLedger::in_memory();
        let records = ledger
            .execute(
                &mut request,
                250,
                || Ok(260),
                |authorization| {
                    let value = disclose_trace_segment(
                        authorization,
                        CompliancePurpose::NetworkTrace,
                        Jurisdiction::Test,
                        &custody,
                        &segment,
                        [1; 32],
                    )?;
                    Ok(DisclosureOutput {
                        record_count: value.len() as u32,
                        value,
                        result_commitment: [8; 32],
                    })
                },
            )
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(records[0], DisclosedTraceRecord::Network(_)));
    }

    #[test]
    fn software_custody_constructor_is_test_jurisdiction_only() {
        assert!(matches!(
            SoftwareCustodyKey::from_bytes(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Us,
                [5; 32],
            ),
            Err(ComplianceError::InvalidKey)
        ));
        assert!(matches!(
            SoftwareCustodyKey::generate(CompliancePurpose::Attribution, Jurisdiction::Eu),
            Err(ComplianceError::InvalidKey)
        ));
    }

    #[test]
    fn decrypted_trace_identity_rejects_wrong_jurisdiction_and_node() {
        let record = |jurisdiction, node_id| {
            DisclosedTraceRecord::Network(TraceRecord {
                jurisdiction,
                operation: NetworkOperation::Publish,
                timestamp_ms: TRACE_EPOCH_DURATION_MS + 101,
                node_id,
                source_ip: TraceIp::V4(Ipv4Addr::new(192, 0, 2, 4)),
                source_port: 9999,
                event_id: Some([2; 32]),
                recipient: Some([3; 32]),
                owner: None,
                size_bytes: 4,
                correlation_id: None,
            })
        };
        assert_eq!(
            validate_decrypted_trace_identity(
                &record(Jurisdiction::Eu, [1; 32]),
                Jurisdiction::Test,
                [1; 32]
            ),
            Err(ComplianceError::SegmentInvalid)
        );
        assert_eq!(
            validate_decrypted_trace_identity(
                &record(Jurisdiction::Test, [2; 32]),
                Jurisdiction::Test,
                [1; 32]
            ),
            Err(ComplianceError::SegmentInvalid)
        );
    }
}
