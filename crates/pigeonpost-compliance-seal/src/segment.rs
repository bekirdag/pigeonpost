//! Bounded, hash-chained trace segment files with crash-safe recovery.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(not(unix))]
use std::fs::{self, File, OpenOptions};
#[cfg(all(test, unix))]
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::io::{Seek, SeekFrom};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use ed25519_dalek::{Signature, VerifyingKey};
use pigeonpost_compliance_format::{
    trace_epoch_contains, ComplianceKeyId, CompliancePurpose, COMPLIANCE_KEY_ID_LEN,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, OpenAccess,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::error::{Result, SealError};
use crate::key::{EpochSealingKey, WrappedEpochKey};
use crate::trace::{IdentityTraceRecord, TraceRecord, IDENTITY_TRACE_RECORD_LEN, TRACE_RECORD_LEN};

const HEADER_MAGIC: &[u8; 8] = b"PPTRACE\0";
const FOOTER_MAGIC: &[u8; 8] = b"PPEND\0\0\0";
const SEGMENT_VERSION: u8 = 1;
/// Exact encoded length of a segment header.
pub const SEGMENT_HEADER_LEN: usize = 348;
/// Exact encoded length of a segment footer.
pub const SEGMENT_FOOTER_LEN: usize = 149;
const FRAME_AAD_DOMAIN: &[u8] = b"pigeonpost/trace-frame-aad/v1";
const FRAME_CHAIN_DOMAIN: &[u8] = b"pigeonpost/trace-frame-chain/v1";
const FOOTER_SIGNATURE_DOMAIN: &[u8] = b"pigeonpost/trace-segment-footer/v1";

/// Hard record bound enforced both while writing and while verifying hostile files.
pub const MAX_SEGMENT_RECORDS: u32 = 10_000;
/// Hard byte bound enforced before a segment is read into memory.
pub const MAX_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
/// Signing boundary for the online node's segment-checkpoint key.
///
/// The trace package needs only this capability and a public key. It does not own or serialize a
/// node secret, and it contains no offline custody secret type.
pub trait SegmentSigner {
    fn verifying_key(&self) -> [u8; 32];
    fn sign(&self, message: &[u8]) -> [u8; 64];
}

impl SegmentSigner for ed25519_dalek::SigningKey {
    fn verifying_key(&self) -> [u8; 32] {
        ed25519_dalek::SigningKey::verifying_key(self).to_bytes()
    }

    fn sign(&self, message: &[u8]) -> [u8; 64] {
        ed25519_dalek::Signer::sign(self, message).to_bytes()
    }
}

/// Public segment header. It contains a wrapped epoch key, never the plaintext epoch key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentHeader {
    segment_id: [u8; 32],
    key_id: ComplianceKeyId,
    opened_at_ms: u64,
    max_records: u32,
    signer_public_key: [u8; 32],
    wrapped_epoch_key: WrappedEpochKey,
}

impl SegmentHeader {
    pub const fn segment_id(&self) -> [u8; 32] {
        self.segment_id
    }

    pub const fn key_id(&self) -> ComplianceKeyId {
        self.key_id
    }

    pub const fn opened_at_ms(&self) -> u64 {
        self.opened_at_ms
    }

    pub const fn max_records(&self) -> u32 {
        self.max_records
    }

    pub const fn signer_public_key(&self) -> [u8; 32] {
        self.signer_public_key
    }

    pub const fn wrapped_epoch_key(&self) -> &WrappedEpochKey {
        &self.wrapped_epoch_key
    }

    /// SHA-256 of the one canonical header encoding.
    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.encode().expect("validated segment header")).into()
    }

    pub fn encode(&self) -> Result<[u8; SEGMENT_HEADER_LEN]> {
        validate_header(self)?;
        let mut out = [0u8; SEGMENT_HEADER_LEN];
        out[..8].copy_from_slice(HEADER_MAGIC);
        out[8] = SEGMENT_VERSION;
        out[9..41].copy_from_slice(&self.segment_id);
        out[41..88].copy_from_slice(&self.key_id.encode().map_err(|_| SealError::Format)?);
        out[88..96].copy_from_slice(&self.opened_at_ms.to_be_bytes());
        out[96..100].copy_from_slice(&self.max_records.to_be_bytes());
        out[100..132].copy_from_slice(&self.signer_public_key);
        out[132..348].copy_from_slice(&self.wrapped_epoch_key.encode()?);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SEGMENT_HEADER_LEN
            || &bytes[..8] != HEADER_MAGIC
            || bytes[8] != SEGMENT_VERSION
        {
            return Err(SealError::CorruptSegment);
        }
        let mut segment_id = [0u8; 32];
        segment_id.copy_from_slice(&bytes[9..41]);
        let key_id = ComplianceKeyId::decode(&bytes[41..41 + COMPLIANCE_KEY_ID_LEN])
            .map_err(|_| SealError::CorruptSegment)?;
        let opened_at_ms = u64::from_be_bytes(bytes[88..96].try_into().expect("fixed slice"));
        let max_records = u32::from_be_bytes(bytes[96..100].try_into().expect("fixed slice"));
        let mut signer_public_key = [0u8; 32];
        signer_public_key.copy_from_slice(&bytes[100..132]);
        let wrapped_epoch_key = WrappedEpochKey::decode(&bytes[132..348])?;
        let header = Self {
            segment_id,
            key_id,
            opened_at_ms,
            max_records,
            signer_public_key,
            wrapped_epoch_key,
        };
        validate_header(&header)?;
        Ok(header)
    }
}

/// One publicly verifiable encrypted frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedFrame {
    sequence: u64,
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
    chain_hash: [u8; 32],
}

impl SealedFrame {
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn nonce(&self) -> [u8; 24] {
        self.nonce
    }

    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    pub const fn chain_hash(&self) -> [u8; 32] {
        self.chain_hash
    }

    /// Reconstruct the AEAD context for approved offline decryption.
    pub fn aad(&self, header: &SegmentHeader, previous_hash: &[u8; 32]) -> Result<Vec<u8>> {
        frame_aad(
            &header.segment_id,
            &header.key_id,
            self.sequence,
            previous_hash,
        )
    }

    pub fn encode(&self, purpose: CompliancePurpose) -> Result<Vec<u8>> {
        let expected_ciphertext = plaintext_len(purpose)? + 16;
        if self.ciphertext.len() != expected_ciphertext {
            return Err(SealError::CorruptSegment);
        }
        let mut out = Vec::with_capacity(frame_len(purpose)?);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out.extend_from_slice(&self.chain_hash);
        Ok(out)
    }

    pub fn decode(bytes: &[u8], purpose: CompliancePurpose) -> Result<Self> {
        decode_frame(bytes, purpose)
    }
}

/// Signed close marker for a complete segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentFooter {
    record_count: u32,
    first_record_hash: [u8; 32],
    final_chain_hash: [u8; 32],
    closed_at_ms: u64,
    signature: [u8; 64],
}

impl SegmentFooter {
    pub const fn record_count(&self) -> u32 {
        self.record_count
    }

    pub const fn first_record_hash(&self) -> [u8; 32] {
        self.first_record_hash
    }

    pub const fn final_chain_hash(&self) -> [u8; 32] {
        self.final_chain_hash
    }

    pub const fn closed_at_ms(&self) -> u64 {
        self.closed_at_ms
    }

    pub const fn signature(&self) -> [u8; 64] {
        self.signature
    }

    /// SHA-256 of the one canonical footer encoding, including its segment signature.
    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }

    pub fn encode(&self) -> [u8; SEGMENT_FOOTER_LEN] {
        let mut out = [0u8; SEGMENT_FOOTER_LEN];
        out[..8].copy_from_slice(FOOTER_MAGIC);
        out[8] = SEGMENT_VERSION;
        out[9..13].copy_from_slice(&self.record_count.to_be_bytes());
        out[13..45].copy_from_slice(&self.first_record_hash);
        out[45..77].copy_from_slice(&self.final_chain_hash);
        out[77..85].copy_from_slice(&self.closed_at_ms.to_be_bytes());
        out[85..149].copy_from_slice(&self.signature);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SEGMENT_FOOTER_LEN
            || &bytes[..8] != FOOTER_MAGIC
            || bytes[8] != SEGMENT_VERSION
        {
            return Err(SealError::CorruptSegment);
        }
        let mut first_record_hash = [0u8; 32];
        first_record_hash.copy_from_slice(&bytes[13..45]);
        let mut final_chain_hash = [0u8; 32];
        final_chain_hash.copy_from_slice(&bytes[45..77]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[85..149]);
        Ok(Self {
            record_count: u32::from_be_bytes(bytes[9..13].try_into().expect("fixed slice")),
            first_record_hash,
            final_chain_hash,
            closed_at_ms: u64::from_be_bytes(bytes[77..85].try_into().expect("fixed slice")),
            signature,
        })
    }
}

/// A complete segment whose frame chain and node signature have been verified without decrypting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSegment {
    pub header: SegmentHeader,
    pub frames: Vec<SealedFrame>,
    pub footer: SegmentFooter,
}

/// Result of recovering a crash-interrupted `.open` segment.
#[derive(Debug)]
pub enum Recovery {
    Resumed(SegmentWriter),
    Finalized(VerifiedSegment),
}

/// Append-only writer for one purpose- and jurisdiction-scoped trace segment.
pub struct SegmentWriter {
    final_path: PathBuf,
    open_path: PathBuf,
    #[cfg(not(unix))]
    file: File,
    #[cfg(unix)]
    file: GuardedFile,
    header: SegmentHeader,
    epoch_key: EpochSealingKey,
    count: u32,
    first_record_hash: [u8; 32],
    previous_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PersistentWriterPlatform {
    supported_persistent_target: bool,
}

impl PersistentWriterPlatform {
    pub(crate) const fn current() -> Self {
        Self {
            supported_persistent_target: cfg!(any(target_os = "linux", target_os = "macos")),
        }
    }

    #[cfg(test)]
    pub(crate) const fn unsupported_for_test() -> Self {
        Self {
            supported_persistent_target: false,
        }
    }
}

pub(crate) fn require_persistent_writer_platform(platform: PersistentWriterPlatform) -> Result<()> {
    if platform.supported_persistent_target {
        Ok(())
    } else {
        Err(unsupported_persistent_writer())
    }
}

fn unsupported_persistent_writer() -> SealError {
    SealError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "persistent compliance operations are supported only on Linux and macOS",
    ))
}

impl SegmentWriter {
    /// Create a new persistent trace segment.
    ///
    /// Persistent segment writing is supported only on Linux and macOS.
    /// Unsupported platforms fail before inspecting or mutating the supplied path.
    pub fn create(
        final_path: impl AsRef<Path>,
        epoch_key: EpochSealingKey,
        compliance_public_key: &[u8; 32],
        signer_public_key: [u8; 32],
        opened_at_ms: u64,
        max_records: u32,
    ) -> Result<Self> {
        Self::create_for_platform(
            PersistentWriterPlatform::current(),
            final_path,
            epoch_key,
            compliance_public_key,
            signer_public_key,
            opened_at_ms,
            max_records,
        )
    }

    fn create_for_platform(
        platform: PersistentWriterPlatform,
        final_path: impl AsRef<Path>,
        epoch_key: EpochSealingKey,
        compliance_public_key: &[u8; 32],
        signer_public_key: [u8; 32],
        opened_at_ms: u64,
        max_records: u32,
    ) -> Result<Self> {
        require_persistent_writer_platform(platform)?;

        #[cfg(not(unix))]
        {
            let _ = (
                final_path.as_ref(),
                epoch_key,
                compliance_public_key,
                signer_public_key,
                opened_at_ms,
                max_records,
            );
            Err(unsupported_persistent_writer())
        }

        #[cfg(unix)]
        {
            if opened_at_ms == 0 || max_records == 0 || max_records > MAX_SEGMENT_RECORDS {
                return Err(SealError::LimitExceeded);
            }
            if !timestamp_in_epoch(opened_at_ms, &epoch_key.key_id()) {
                return Err(SealError::InvalidRecord);
            }
            parse_verifying_key(&signer_public_key)?;
            let requested_final_path = final_path.as_ref().to_path_buf();

            let mut segment_id = [0u8; 32];
            OsRng.fill_bytes(&mut segment_id);
            let wrapped_epoch_key = WrappedEpochKey::wrap(&epoch_key, compliance_public_key)?;
            let header = SegmentHeader {
                segment_id,
                key_id: epoch_key.key_id(),
                opened_at_ms,
                max_records,
                signer_public_key,
                wrapped_epoch_key,
            };
            let header_bytes = header.encode()?;
            let (final_path, open_path, file) = {
                let requested_parent = requested_final_path
                    .parent()
                    .ok_or_else(invalid_segment_path)?;
                let parent = secure_artifact_parent(requested_parent)?;
                let final_name = leaf_name_for_path(&requested_final_path)?;
                if parent
                    .entry_metadata(&final_name)
                    .map_err(map_segment_custody_error)?
                    .is_some()
                {
                    return Err(SealError::AlreadyExists);
                }
                let open_name = open_leaf_for(&final_name)?;
                let mut file = parent
                    .create_file(&open_name, FilePolicy::private(MAX_SEGMENT_BYTES))
                    .map_err(map_segment_custody_error)?;
                file.write_all(&header_bytes)?;
                file.sync_all().map_err(map_segment_custody_error)?;
                parent.sync().map_err(map_segment_custody_error)?;
                let final_path = parent.absolute_path().join(final_name.as_os_str());
                let open_path = parent.absolute_path().join(open_name.as_os_str());
                (final_path, open_path, file)
            };
            let previous_hash = Sha256::digest(header_bytes).into();
            Ok(Self {
                final_path,
                open_path,
                file,
                header,
                epoch_key,
                count: 0,
                first_record_hash: [0u8; 32],
                previous_hash,
            })
        }
    }

    pub fn header(&self) -> &SegmentHeader {
        &self.header
    }

    pub const fn record_count(&self) -> u32 {
        self.count
    }

    pub fn append_network(&mut self, record: &TraceRecord) -> Result<[u8; 32]> {
        let chain_hash = self.append_network_buffered(record)?;
        self.sync_data()?;
        Ok(chain_hash)
    }

    /// Append one network record without acknowledging durability yet.
    ///
    /// This narrow API exists for a single-owner group-commit loop. Callers **must not** report
    /// success for this record until a later [`Self::sync_data`] succeeds. Ordinary callers should
    /// use [`Self::append_network`], which retains per-record durability.
    pub fn append_network_buffered(&mut self, record: &TraceRecord) -> Result<[u8; 32]> {
        if self.header.key_id.purpose != CompliancePurpose::NetworkTrace
            || record.jurisdiction != self.header.key_id.jurisdiction
        {
            return Err(SealError::WrongPurpose);
        }
        if !timestamp_in_epoch(record.timestamp_ms, &self.header.key_id) {
            return Err(SealError::InvalidRecord);
        }
        let mut plaintext = record.encode()?;
        let result = self.append_plaintext(&plaintext);
        plaintext.zeroize();
        result
    }

    pub fn append_identity(&mut self, record: &IdentityTraceRecord) -> Result<[u8; 32]> {
        let chain_hash = self.append_identity_buffered(record)?;
        self.sync_data()?;
        Ok(chain_hash)
    }

    /// Append one identity record without acknowledging durability yet.
    ///
    /// This is the identity-purpose counterpart to [`Self::append_network_buffered`]. The owner
    /// must not acknowledge the record until a later [`Self::sync_data`] succeeds.
    pub fn append_identity_buffered(&mut self, record: &IdentityTraceRecord) -> Result<[u8; 32]> {
        if self.header.key_id.purpose != CompliancePurpose::IdentityTrace
            || record.jurisdiction != self.header.key_id.jurisdiction
        {
            return Err(SealError::WrongPurpose);
        }
        if !timestamp_in_epoch(record.timestamp_ms, &self.header.key_id) {
            return Err(SealError::InvalidRecord);
        }
        let mut plaintext = record.encode()?;
        let result = self.append_plaintext(&plaintext);
        plaintext.zeroize();
        result
    }

    /// Durably flush every frame appended since the previous successful sync.
    ///
    /// A group-commit owner may acknowledge all frames in its exact batch only after this returns
    /// successfully. A failure leaves the writer unusable to that owner; crash recovery will keep
    /// only the valid on-disk prefix.
    pub fn sync_data(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            self.file
                .verify_named()
                .map_err(map_segment_custody_error)?;
            self.file.file().sync_data()?;
        }
        #[cfg(not(unix))]
        self.file.sync_data()?;
        Ok(())
    }

    fn append_plaintext(&mut self, plaintext: &[u8]) -> Result<[u8; 32]> {
        if self.count >= self.header.max_records || self.count >= MAX_SEGMENT_RECORDS {
            return Err(SealError::SegmentFull);
        }
        let sequence = u64::from(self.count);
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let aad = frame_aad(
            &self.header.segment_id,
            &self.header.key_id,
            sequence,
            &self.previous_hash,
        )?;
        let ciphertext = XChaCha20Poly1305::new(self.epoch_key.secret().into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::Crypto)?;
        let chain_hash = frame_chain_hash(sequence, &self.previous_hash, &nonce, &ciphertext);
        let mut frame = Vec::with_capacity(8 + 24 + ciphertext.len() + 32);
        frame.extend_from_slice(&sequence.to_be_bytes());
        frame.extend_from_slice(&nonce);
        frame.extend_from_slice(&ciphertext);
        frame.extend_from_slice(&chain_hash);
        self.file.write_all(&frame)?;
        self.count += 1;
        if self.count == 1 {
            self.first_record_hash = chain_hash;
        }
        self.previous_hash = chain_hash;
        Ok(chain_hash)
    }

    /// Write and fsync the signed close marker, then atomically publish the final path.
    pub fn finalize(
        self,
        closed_at_ms: u64,
        signer: &impl SegmentSigner,
    ) -> Result<VerifiedSegment> {
        let final_path = self.final_path.clone();
        self.finalize_durable(closed_at_ms, signer)?;
        verify_segment(final_path)
    }

    /// Durably publish a segment without rereading every frame on the latency-sensitive writer
    /// path. The writer has already authenticated each appended frame and signs the final chain;
    /// auditors can call [`verify_segment`] independently after publication.
    pub fn finalize_durable(
        mut self,
        closed_at_ms: u64,
        signer: &impl SegmentSigner,
    ) -> Result<SegmentFooter> {
        validate_opened_segment(&self.file, &self.open_path, true, MAX_SEGMENT_BYTES)?;
        if signer.verifying_key() != self.header.signer_public_key {
            return Err(SealError::InvalidKey);
        }
        if closed_at_ms < self.header.opened_at_ms
            || !timestamp_in_epoch(closed_at_ms, &self.header.key_id)
        {
            return Err(SealError::InvalidRecord);
        }
        let unsigned = footer_signature_preimage(
            &self.header.hash(),
            self.count,
            &self.first_record_hash,
            &self.previous_hash,
            closed_at_ms,
        );
        let footer = SegmentFooter {
            record_count: self.count,
            first_record_hash: self.first_record_hash,
            final_chain_hash: self.previous_hash,
            closed_at_ms,
            signature: signer.sign(&unsigned),
        };
        self.file.write_all(&footer.encode())?;
        #[cfg(unix)]
        {
            self.file.sync_all().map_err(map_segment_custody_error)?;
            let final_name = leaf_name_for_path(&self.final_path)?;
            let parent = self.file.parent().clone();
            let published = parent
                .publish_no_replace(self.file, &parent, &final_name)
                .map_err(map_segment_custody_error)?;
            if !named_segment_matches(&published, &self.final_path) {
                return Err(SealError::CorruptSegment);
            }
        }
        #[cfg(not(unix))]
        {
            self.file.sync_all()?;
            rename_no_replace(&self.open_path, &self.final_path)?;
            if !named_segment_matches(&self.file, &self.final_path) {
                return Err(SealError::CorruptSegment);
            }
            sync_parent(self.final_path.parent().expect("validated parent"))?;
        }
        Ok(footer)
    }
}

impl core::fmt::Debug for SegmentWriter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SegmentWriter")
            .field("path", &"<withheld>")
            .field("header", &self.header)
            .field("epoch_key", &"<withheld>")
            .field("count", &self.count)
            .finish()
    }
}

/// Verify a complete segment using only its public header and signer key.
pub fn verify_segment(path: impl AsRef<Path>) -> Result<VerifiedSegment> {
    verify_segment_with_privacy(path.as_ref(), false)
}

/// Verify a complete segment while requiring owner-only, single-link local custody.
///
/// Offline consumers use this stricter entry point so a configured path cannot alias another
/// artifact through a hard link or expose retained ciphertext through group/world permissions.
pub fn verify_owner_only_segment(path: impl AsRef<Path>) -> Result<VerifiedSegment> {
    verify_owner_only_segment_for_platform(PersistentWriterPlatform::current(), path)
}

fn verify_owner_only_segment_for_platform(
    platform: PersistentWriterPlatform,
    path: impl AsRef<Path>,
) -> Result<VerifiedSegment> {
    require_persistent_writer_platform(platform)?;
    verify_segment_with_privacy(path.as_ref(), true)
}

fn verify_segment_with_privacy(path: &Path, private: bool) -> Result<VerifiedSegment> {
    let mut file = open_segment_file(path, false, private)?;
    validate_opened_segment(&file, path, private, MAX_SEGMENT_BYTES)?;
    #[cfg(unix)]
    let metadata_len = file.metadata().map_err(map_segment_custody_error)?.len;
    #[cfg(not(unix))]
    let metadata_len = file.metadata()?.len();
    if metadata_len > MAX_SEGMENT_BYTES {
        return Err(SealError::LimitExceeded);
    }
    let mut bytes = Vec::with_capacity(metadata_len as usize);
    (&mut file)
        .take(MAX_SEGMENT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata_len
        || bytes.len() as u64 > MAX_SEGMENT_BYTES
        || !named_segment_matches(&file, path)
    {
        return Err(SealError::CorruptSegment);
    }
    verify_segment_bytes(&bytes)
}

/// Resume a valid partial frame tail, or publish a fully fsynced footer left before rename.
///
/// Persistent recovery is supported only on Linux and macOS. Unsupported
/// platforms fail before inspecting or mutating the supplied path.
pub fn recover_segment(
    final_path: impl AsRef<Path>,
    epoch_key: EpochSealingKey,
) -> Result<Recovery> {
    recover_segment_for_platform(PersistentWriterPlatform::current(), final_path, epoch_key)
}

fn recover_segment_for_platform(
    platform: PersistentWriterPlatform,
    final_path: impl AsRef<Path>,
    epoch_key: EpochSealingKey,
) -> Result<Recovery> {
    require_persistent_writer_platform(platform)?;

    #[cfg(not(unix))]
    {
        let _ = (final_path.as_ref(), epoch_key);
        Err(unsupported_persistent_writer())
    }

    #[cfg(unix)]
    {
        let final_path = final_path.as_ref().to_path_buf();
        match verify_segment(&final_path) {
            Ok(segment) => return Ok(Recovery::Finalized(segment)),
            Err(SealError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let open_path = open_path_for(&final_path);
        let mut file = open_segment_file(&open_path, true, true)?;
        #[cfg(unix)]
        let metadata_len = file.metadata().map_err(map_segment_custody_error)?.len;
        #[cfg(not(unix))]
        let metadata_len = file.metadata()?.len();
        if metadata_len > MAX_SEGMENT_BYTES {
            return Err(SealError::LimitExceeded);
        }
        let mut bytes = Vec::with_capacity(metadata_len as usize);
        (&mut file)
            .take(MAX_SEGMENT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != metadata_len
            || bytes.len() as u64 > MAX_SEGMENT_BYTES
            || !named_segment_matches(&file, &open_path)
        {
            return Err(SealError::CorruptSegment);
        }
        if bytes.len() < SEGMENT_HEADER_LEN {
            return Err(SealError::CorruptSegment);
        }
        let header = SegmentHeader::decode(&bytes[..SEGMENT_HEADER_LEN])?;
        if header.key_id != epoch_key.key_id()
            || header.wrapped_epoch_key.epoch_key_commitment() != epoch_key.commitment()
        {
            return Err(SealError::InvalidKey);
        }

        let frame_len = frame_len(header.key_id.purpose)?;
        let mut offset = SEGMENT_HEADER_LEN;
        let mut count = 0u32;
        let mut first_record_hash = [0u8; 32];
        let mut previous_hash = header.hash();
        while offset < bytes.len() {
            let remaining = bytes.len() - offset;
            if remaining >= 8 && &bytes[offset..offset + 8] == FOOTER_MAGIC {
                if remaining == SEGMENT_FOOTER_LEN {
                    let verified = verify_segment_bytes(&bytes)?;
                    #[cfg(unix)]
                    {
                        let final_name = leaf_name_for_path(&final_path)?;
                        let parent = file.parent().clone();
                        let published = parent
                            .publish_no_replace(file, &parent, &final_name)
                            .map_err(map_segment_custody_error)?;
                        if !named_segment_matches(&published, &final_path) {
                            return Err(SealError::CorruptSegment);
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        rename_no_replace(&open_path, &final_path)?;
                        if !named_segment_matches(&file, &final_path) {
                            return Err(SealError::CorruptSegment);
                        }
                        sync_parent(final_path.parent().ok_or_else(invalid_segment_path)?)?;
                    }
                    return Ok(Recovery::Finalized(verified));
                }
                if remaining < SEGMENT_FOOTER_LEN {
                    break;
                }
                return Err(SealError::CorruptSegment);
            }
            if remaining < frame_len {
                break;
            }
            let frame = decode_frame(&bytes[offset..offset + frame_len], header.key_id.purpose)?;
            verify_frame(&frame, count, &previous_hash)?;
            count = count.checked_add(1).ok_or(SealError::LimitExceeded)?;
            if count > header.max_records || count > MAX_SEGMENT_RECORDS {
                return Err(SealError::LimitExceeded);
            }
            if count == 1 {
                first_record_hash = frame.chain_hash;
            }
            previous_hash = frame.chain_hash;
            offset += frame_len;
        }
        #[cfg(unix)]
        file.file().set_len(offset as u64)?;
        #[cfg(not(unix))]
        file.set_len(offset as u64)?;
        file.seek(SeekFrom::Start(offset as u64))?;
        #[cfg(unix)]
        file.sync_all().map_err(map_segment_custody_error)?;
        #[cfg(not(unix))]
        file.sync_all()?;
        Ok(Recovery::Resumed(SegmentWriter {
            final_path,
            open_path,
            file,
            header,
            epoch_key,
            count,
            first_record_hash,
            previous_hash,
        }))
    }
}

fn verify_segment_bytes(bytes: &[u8]) -> Result<VerifiedSegment> {
    if bytes.len() < SEGMENT_HEADER_LEN + SEGMENT_FOOTER_LEN
        || bytes.len() as u64 > MAX_SEGMENT_BYTES
    {
        return Err(SealError::CorruptSegment);
    }
    let header = SegmentHeader::decode(&bytes[..SEGMENT_HEADER_LEN])?;
    let footer = SegmentFooter::decode(&bytes[bytes.len() - SEGMENT_FOOTER_LEN..])?;
    if footer.record_count > header.max_records || footer.record_count > MAX_SEGMENT_RECORDS {
        return Err(SealError::LimitExceeded);
    }
    let frame_len = frame_len(header.key_id.purpose)?;
    let expected_len = SEGMENT_HEADER_LEN
        .checked_add(
            (footer.record_count as usize)
                .checked_mul(frame_len)
                .ok_or(SealError::LimitExceeded)?,
        )
        .and_then(|length| length.checked_add(SEGMENT_FOOTER_LEN))
        .ok_or(SealError::LimitExceeded)?;
    if bytes.len() != expected_len || footer.closed_at_ms < header.opened_at_ms {
        return Err(SealError::CorruptSegment);
    }
    let mut frames = Vec::with_capacity(footer.record_count as usize);
    let mut previous_hash = header.hash();
    let mut offset = SEGMENT_HEADER_LEN;
    for sequence in 0..footer.record_count {
        let frame = decode_frame(&bytes[offset..offset + frame_len], header.key_id.purpose)?;
        verify_frame(&frame, sequence, &previous_hash)?;
        previous_hash = frame.chain_hash;
        frames.push(frame);
        offset += frame_len;
    }
    let first = frames.first().map_or([0u8; 32], |frame| frame.chain_hash);
    if first != footer.first_record_hash || previous_hash != footer.final_chain_hash {
        return Err(SealError::CorruptSegment);
    }
    let verifying_key = parse_verifying_key(&header.signer_public_key)?;
    let preimage = footer_signature_preimage(
        &header.hash(),
        footer.record_count,
        &footer.first_record_hash,
        &footer.final_chain_hash,
        footer.closed_at_ms,
    );
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&footer.signature))
        .map_err(|_| SealError::BadSignature)?;
    Ok(VerifiedSegment {
        header,
        frames,
        footer,
    })
}

fn decode_frame(bytes: &[u8], purpose: CompliancePurpose) -> Result<SealedFrame> {
    let ciphertext_len = plaintext_len(purpose)? + 16;
    if bytes.len() != 8 + 24 + ciphertext_len + 32 {
        return Err(SealError::CorruptSegment);
    }
    let sequence = u64::from_be_bytes(bytes[..8].try_into().expect("fixed slice"));
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&bytes[8..32]);
    let ciphertext = bytes[32..32 + ciphertext_len].to_vec();
    let mut chain_hash = [0u8; 32];
    chain_hash.copy_from_slice(&bytes[32 + ciphertext_len..]);
    Ok(SealedFrame {
        sequence,
        nonce,
        ciphertext,
        chain_hash,
    })
}

fn verify_frame(
    frame: &SealedFrame,
    expected_sequence: u32,
    previous_hash: &[u8; 32],
) -> Result<()> {
    if frame.sequence != u64::from(expected_sequence)
        || frame.chain_hash
            != frame_chain_hash(
                frame.sequence,
                previous_hash,
                &frame.nonce,
                &frame.ciphertext,
            )
    {
        return Err(SealError::CorruptSegment);
    }
    Ok(())
}

fn frame_len(purpose: CompliancePurpose) -> Result<usize> {
    Ok(8 + 24 + plaintext_len(purpose)? + 16 + 32)
}

fn plaintext_len(purpose: CompliancePurpose) -> Result<usize> {
    match purpose {
        CompliancePurpose::NetworkTrace => Ok(TRACE_RECORD_LEN),
        CompliancePurpose::IdentityTrace => Ok(IDENTITY_TRACE_RECORD_LEN),
        CompliancePurpose::Attribution => Err(SealError::WrongPurpose),
    }
}

fn validate_header(header: &SegmentHeader) -> Result<()> {
    if header.segment_id == [0u8; 32]
        || header.opened_at_ms == 0
        || !timestamp_in_epoch(header.opened_at_ms, &header.key_id)
        || header.max_records == 0
        || header.max_records > MAX_SEGMENT_RECORDS
        || header.signer_public_key == [0u8; 32]
        || header.wrapped_epoch_key.key_id() != header.key_id
    {
        return Err(SealError::CorruptSegment);
    }
    plaintext_len(header.key_id.purpose)?;
    parse_verifying_key(&header.signer_public_key)?;
    Ok(())
}

fn parse_verifying_key(bytes: &[u8; 32]) -> Result<VerifyingKey> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| SealError::InvalidKey)?;
    if key.is_weak() {
        return Err(SealError::InvalidKey);
    }
    Ok(key)
}

fn timestamp_in_epoch(timestamp_ms: u64, key_id: &ComplianceKeyId) -> bool {
    trace_epoch_contains(key_id, timestamp_ms).unwrap_or(false)
}

fn frame_aad(
    segment_id: &[u8; 32],
    key_id: &ComplianceKeyId,
    sequence: u64,
    previous_hash: &[u8; 32],
) -> Result<Vec<u8>> {
    let encoded_key_id = key_id.encode().map_err(|_| SealError::Format)?;
    let mut aad = Vec::with_capacity(FRAME_AAD_DOMAIN.len() + 32 + COMPLIANCE_KEY_ID_LEN + 8 + 32);
    aad.extend_from_slice(FRAME_AAD_DOMAIN);
    aad.extend_from_slice(segment_id);
    aad.extend_from_slice(&encoded_key_id);
    aad.extend_from_slice(&sequence.to_be_bytes());
    aad.extend_from_slice(previous_hash);
    Ok(aad)
}

fn frame_chain_hash(
    sequence: u64,
    previous_hash: &[u8; 32],
    nonce: &[u8; 24],
    ciphertext: &[u8],
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(FRAME_CHAIN_DOMAIN);
    hash.update(sequence.to_be_bytes());
    hash.update(previous_hash);
    hash.update(nonce);
    hash.update(ciphertext);
    hash.finalize().into()
}

fn footer_signature_preimage(
    header_hash: &[u8; 32],
    record_count: u32,
    first_record_hash: &[u8; 32],
    final_chain_hash: &[u8; 32],
    closed_at_ms: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(FOOTER_SIGNATURE_DOMAIN.len() + 32 + 4 + 32 + 32 + 8);
    out.extend_from_slice(FOOTER_SIGNATURE_DOMAIN);
    out.extend_from_slice(header_hash);
    out.extend_from_slice(&record_count.to_be_bytes());
    out.extend_from_slice(first_record_hash);
    out.extend_from_slice(final_chain_hash);
    out.extend_from_slice(&closed_at_ms.to_be_bytes());
    out
}

#[cfg(any(unix, test))]
fn open_path_for(final_path: &Path) -> PathBuf {
    let mut name = final_path.as_os_str().to_os_string();
    name.push(".open");
    PathBuf::from(name)
}

#[cfg(unix)]
fn invalid_segment_path() -> SealError {
    SealError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "segment path must have a parent and leaf name",
    ))
}

#[cfg(unix)]
fn leaf_name_for_path(path: &Path) -> Result<LeafName> {
    let name = path.file_name().ok_or_else(invalid_segment_path)?;
    LeafName::new(name).map_err(map_segment_custody_error)
}

#[cfg(unix)]
fn open_leaf_for(final_name: &LeafName) -> Result<LeafName> {
    let mut name = final_name.as_os_str().to_os_string();
    name.push(".open");
    LeafName::new(name).map_err(map_segment_custody_error)
}

#[cfg(unix)]
pub(crate) fn map_segment_custody_error(error: CustodyError) -> SealError {
    match error {
        CustodyError::Io(error) => SealError::Io(error),
        CustodyError::NotFound => SealError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "custody object was not found",
        )),
        CustodyError::AlreadyExists => SealError::AlreadyExists,
        CustodyError::LimitExceeded(_) => SealError::LimitExceeded,
        CustodyError::InvalidPath(_)
        | CustodyError::UnsafeAncestor(_)
        | CustodyError::UnsafeDirectory(_)
        | CustodyError::UnsafeFile(_)
        | CustodyError::Unsupported(_) => SealError::CorruptSegment,
    }
}

#[cfg(unix)]
fn open_segment_file(path: &Path, writable: bool, private: bool) -> Result<GuardedFile> {
    let parent_path = path.parent().ok_or_else(invalid_segment_path)?;
    let name = leaf_name_for_path(path)?;
    let directory_policy = if writable {
        DirPolicy::private_mutable()
    } else if private {
        DirPolicy::private()
    } else {
        DirPolicy::trusted()
    };
    let directory = GuardedDir::open_existing(parent_path, directory_policy)
        .map_err(map_segment_custody_error)?;
    let file_policy = if writable || private {
        FilePolicy::private(MAX_SEGMENT_BYTES)
    } else {
        FilePolicy::trusted(MAX_SEGMENT_BYTES)
    };
    let access = if writable {
        OpenAccess::ReadWrite
    } else {
        OpenAccess::ReadOnly
    };
    let file = directory
        .open_file(&name, access, file_policy)
        .map_err(map_segment_custody_error)?;
    validate_opened_segment(&file, path, private, MAX_SEGMENT_BYTES)?;
    Ok(file)
}

#[cfg(windows)]
fn open_segment_file(path: &Path, writable: bool, _private: bool) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    validate_opened_segment(&file, path, writable, MAX_SEGMENT_BYTES)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_segment_file(path: &Path, writable: bool, _private: bool) -> Result<File> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(SealError::CorruptSegment);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    let file = options.open(path)?;
    validate_opened_segment(&file, path, writable, MAX_SEGMENT_BYTES)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_opened_segment(
    file: &GuardedFile,
    _path: &Path,
    _private: bool,
    max_bytes: u64,
) -> Result<()> {
    let metadata = file.metadata().map_err(map_segment_custody_error)?;
    if metadata.len > max_bytes {
        return Err(SealError::LimitExceeded);
    }
    file.verify_named().map_err(map_segment_custody_error)
}

#[cfg(not(unix))]
fn validate_opened_segment(file: &File, path: &Path, private: bool, max_bytes: u64) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max_bytes || !named_segment_matches(file, path) {
        return Err(SealError::CorruptSegment);
    }
    let _ = private;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(SealError::CorruptSegment);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn named_segment_matches(file: &GuardedFile, _path: &Path) -> bool {
    file.verify_named().is_ok()
}

#[cfg(not(unix))]
fn named_segment_matches(file: &File, path: &Path) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

#[cfg(unix)]
pub(crate) fn secure_artifact_parent(parent: &Path) -> Result<GuardedDir> {
    GuardedDir::create_private(parent).map_err(map_segment_custody_error)
}

#[cfg(not(unix))]
pub(crate) fn secure_artifact_parent(parent: &Path) -> Result<()> {
    fs::create_dir_all(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SealError::CorruptSegment);
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn rename_no_replace(source: &Path, destination: &Path) -> Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => {
            fs::remove_file(source)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(SealError::AlreadyExists)
        }
        Err(error) => Err(SealError::Io(error)),
    }
}

#[cfg(not(unix))]
pub(crate) fn sync_parent(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[cfg(unix)]
    use std::process::Command;

    use curve25519_dalek::montgomery::MontgomeryPoint;
    use ed25519_dalek::SigningKey;
    use pigeonpost_compliance_format::Jurisdiction;
    use tempfile::tempdir;

    use super::*;
    use crate::trace::{NetworkOperation, TraceIp};

    const TEST_EPOCH_MS: u64 = 19_000 * pigeonpost_compliance_format::TRACE_EPOCH_DURATION_MS;

    fn key_id(purpose: CompliancePurpose) -> ComplianceKeyId {
        ComplianceKeyId::new(purpose, Jurisdiction::Test, [7; 32], TEST_EPOCH_MS, 1)
    }

    fn publish() -> TraceRecord {
        TraceRecord {
            jurisdiction: Jurisdiction::Test,
            operation: NetworkOperation::Publish,
            timestamp_ms: TEST_EPOCH_MS + 100,
            node_id: [1; 32],
            source_ip: TraceIp::V4(Ipv4Addr::new(198, 51, 100, 7)),
            source_port: 4242,
            event_id: Some([2; 32]),
            recipient: Some([3; 32]),
            owner: None,
            size_bytes: 512,
            correlation_id: None,
        }
    }

    fn setup(path: &Path) -> (SegmentWriter, SigningKey, [u8; 32]) {
        make_private(path.parent().expect("test segment parent"));
        let signer = SigningKey::from_bytes(&[9; 32]);
        let epoch_bytes = [8u8; 32];
        let epoch =
            EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), epoch_bytes)
                .unwrap();
        let custody_secret = [6u8; 32];
        let custody_public = MontgomeryPoint::mul_base_clamped(custody_secret).to_bytes();
        let writer = SegmentWriter::create(
            path,
            epoch,
            &custody_public,
            signer.verifying_key().to_bytes(),
            TEST_EPOCH_MS,
            3,
        )
        .unwrap();
        (writer, signer, epoch_bytes)
    }

    #[cfg(unix)]
    fn make_private(directory: &Path) {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_private(_directory: &Path) {}

    #[cfg(unix)]
    fn create_writer(path: &Path) -> Result<SegmentWriter> {
        let signer = SigningKey::from_bytes(&[9; 32]);
        let epoch =
            EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), [8u8; 32])?;
        let custody_public = MontgomeryPoint::mul_base_clamped([6u8; 32]).to_bytes();
        SegmentWriter::create(
            path,
            epoch,
            &custody_public,
            signer.verifying_key().to_bytes(),
            TEST_EPOCH_MS,
            3,
        )
    }

    fn assert_unsupported_platform(error: SealError) {
        assert!(matches!(
            error,
            SealError::Io(error) if error.kind() == std::io::ErrorKind::Unsupported
        ));
    }

    struct PanicPath;

    impl AsRef<Path> for PanicPath {
        fn as_ref(&self) -> &Path {
            panic!("unsupported owner-only verifier must not inspect its path")
        }
    }

    #[test]
    fn persistent_platform_capability_matches_the_linux_macos_release_matrix() {
        assert_eq!(
            require_persistent_writer_platform(PersistentWriterPlatform::current()).is_ok(),
            cfg!(any(target_os = "linux", target_os = "macos"))
        );
    }

    #[test]
    fn unsupported_platform_gate_precedes_owner_only_path_inspection() {
        let error = verify_owner_only_segment_for_platform(
            PersistentWriterPlatform::unsupported_for_test(),
            PanicPath,
        )
        .unwrap_err();
        assert_unsupported_platform(error);
    }

    #[test]
    fn unsupported_platform_gate_precedes_segment_creation() {
        let temp = tempdir().unwrap();
        let segment_directory = temp.path().join("must-not-exist");
        let segment_path = segment_directory.join("trace.ppseg");
        let signer = SigningKey::from_bytes(&[9; 32]);
        let epoch = EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), [8u8; 32])
            .unwrap();
        let custody_public = MontgomeryPoint::mul_base_clamped([6u8; 32]).to_bytes();

        let error = SegmentWriter::create_for_platform(
            PersistentWriterPlatform::unsupported_for_test(),
            &segment_path,
            epoch,
            &custody_public,
            signer.verifying_key().to_bytes(),
            TEST_EPOCH_MS,
            3,
        )
        .unwrap_err();

        assert_unsupported_platform(error);
        assert!(!segment_directory.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_platform_gate_precedes_recovery_truncation() {
        let temp = tempdir().unwrap();
        let segment_path = temp.path().join("trace.ppseg");
        let (mut writer, _, epoch_bytes) = setup(&segment_path);
        writer.append_network(&publish()).unwrap();
        let open_path = open_path_for(&segment_path);
        drop(writer);
        OpenOptions::new()
            .append(true)
            .open(&open_path)
            .unwrap()
            .write_all(&[1, 2, 3, 4])
            .unwrap();
        let before = fs::read(&open_path).unwrap();
        let epoch =
            EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), epoch_bytes)
                .unwrap();

        let error = recover_segment_for_platform(
            PersistentWriterPlatform::unsupported_for_test(),
            &segment_path,
            epoch,
        )
        .unwrap_err();

        assert_unsupported_platform(error);
        assert_eq!(fs::read(&open_path).unwrap(), before);
        assert!(!segment_path.exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn complete_segment_verifies_without_decrypting() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, _) = setup(&path);
        writer.append_network(&publish()).unwrap();
        writer.append_network(&publish()).unwrap();
        let segment = writer.finalize(TEST_EPOCH_MS + 500, &signer).unwrap();
        assert_eq!(segment.frames.len(), 2);
        assert_eq!(verify_segment(&path).unwrap(), segment);
        assert!(!open_path_for(&path).exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn buffered_batch_decrypts_only_after_explicit_durable_sync() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, epoch_bytes) = setup(&path);
        let first = publish();
        let mut second = publish();
        second.timestamp_ms += 1;
        second.event_id = Some([4; 32]);

        writer.append_network_buffered(&first).unwrap();
        writer.append_network_buffered(&second).unwrap();
        writer.sync_data().unwrap();
        let segment = writer.finalize(TEST_EPOCH_MS + 500, &signer).unwrap();

        let cipher = XChaCha20Poly1305::new((&epoch_bytes).into());
        let mut previous_hash = segment.header.hash();
        for (frame, expected) in segment.frames.iter().zip([first, second]) {
            let aad = frame.aad(&segment.header, &previous_hash).unwrap();
            let plaintext = cipher
                .decrypt(
                    XNonce::from_slice(&frame.nonce()),
                    Payload {
                        msg: frame.ciphertext(),
                        aad: &aad,
                    },
                )
                .unwrap();
            assert_eq!(TraceRecord::decode(&plaintext).unwrap(), expected);
            previous_hash = frame.chain_hash();
        }
        assert_eq!(segment.footer.record_count(), 2);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn weak_segment_signing_keys_are_rejected() {
        let temp = tempdir().unwrap();
        let epoch = EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), [8u8; 32])
            .unwrap();
        let custody_public = MontgomeryPoint::mul_base_clamped([6u8; 32]).to_bytes();
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert!(matches!(
            SegmentWriter::create(
                temp.path().join("weak.ppseg"),
                epoch,
                &custody_public,
                weak,
                TEST_EPOCH_MS,
                3,
            ),
            Err(SealError::InvalidKey)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn public_verifier_rejects_tampering_and_trailing_bytes() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, _) = setup(&path);
        writer.append_network(&publish()).unwrap();
        writer.finalize(TEST_EPOCH_MS + 500, &signer).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[SEGMENT_HEADER_LEN + 40] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(verify_segment(&path).is_err());
        bytes.push(0);
        fs::write(&path, &bytes).unwrap();
        assert!(verify_segment(&path).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn recovery_truncates_only_an_incomplete_tail() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, epoch_bytes) = setup(&path);
        writer.append_network(&publish()).unwrap();
        let open = open_path_for(&path);
        drop(writer);
        OpenOptions::new()
            .append(true)
            .open(&open)
            .unwrap()
            .write_all(&[1, 2, 3, 4])
            .unwrap();
        let epoch =
            EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), epoch_bytes)
                .unwrap();
        let Recovery::Resumed(mut writer) = recover_segment(&path, epoch).unwrap() else {
            panic!("partial tail must resume");
        };
        assert_eq!(writer.record_count(), 1);
        writer.append_network(&publish()).unwrap();
        assert_eq!(
            writer
                .finalize(TEST_EPOCH_MS + 1_000, &signer)
                .unwrap()
                .frames
                .len(),
            2
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn recovery_never_truncates_a_complete_corrupt_frame() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, _, epoch_bytes) = setup(&path);
        writer.append_network(&publish()).unwrap();
        let open = open_path_for(&path);
        drop(writer);
        let mut bytes = fs::read(&open).unwrap();
        bytes[SEGMENT_HEADER_LEN + 40] ^= 1;
        fs::write(&open, bytes).unwrap();
        let epoch =
            EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), epoch_bytes)
                .unwrap();
        assert!(matches!(
            recover_segment(&path, epoch),
            Err(SealError::CorruptSegment)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn recovery_publishes_a_complete_footer_left_before_rename() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, epoch_bytes) = setup(&path);
        writer.append_network(&publish()).unwrap();
        writer.finalize(TEST_EPOCH_MS + 1_000, &signer).unwrap();
        let open = open_path_for(&path);
        fs::rename(&path, &open).unwrap();
        let epoch =
            EpochSealingKey::from_bytes(key_id(CompliancePurpose::NetworkTrace), epoch_bytes)
                .unwrap();
        let Recovery::Finalized(segment) = recover_segment(&path, epoch).unwrap() else {
            panic!("complete footer must be published");
        };
        assert_eq!(segment.frames.len(), 1);
        assert!(path.exists());
        assert!(!open.exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn finalize_never_replaces_an_existing_destination() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, _) = setup(&path);
        writer.append_network(&publish()).unwrap();
        fs::write(&path, b"do-not-replace").unwrap();
        assert!(matches!(
            writer.finalize_durable(TEST_EPOCH_MS + 1_000, &signer),
            Err(SealError::AlreadyExists)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"do-not-replace");
    }

    #[cfg(unix)]
    #[test]
    fn segment_paths_reject_symlinks_and_open_file_hardlinks() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, signer, _) = setup(&path);
        writer.append_network(&publish()).unwrap();
        let open = open_path_for(&path);
        let alias = temp.path().join("trace.alias");
        fs::hard_link(&open, &alias).unwrap();
        assert!(matches!(
            writer.finalize_durable(TEST_EPOCH_MS + 1_000, &signer),
            Err(SealError::CorruptSegment)
        ));

        let target = temp.path().join("target.ppseg");
        fs::write(&target, b"not-a-segment").unwrap();
        let link = temp.path().join("trace.link");
        symlink(&target, &link).unwrap();
        assert!(verify_segment(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_ancestors_fail_before_creating_any_segment_artifact() {
        let temp = tempdir().unwrap();
        make_private(temp.path());

        let actual = temp.path().join("actual");
        fs::create_dir(&actual).unwrap();
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = temp.path().join("alias");
        symlink(&actual, &alias).unwrap();
        assert!(create_writer(&alias.join("trace.ppseg")).is_err());
        assert_eq!(fs::read_dir(&actual).unwrap().count(), 0);

        let mutable = temp.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o777)).unwrap();
        let nested = mutable.join("trace-store");
        assert!(create_writer(&nested.join("trace.ppseg")).is_err());
        assert!(!nested.exists());
        assert_eq!(
            fs::metadata(&mutable).unwrap().permissions().mode() & 0o7777,
            0o777
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_file_and_directory_replacement_never_redirect_a_writer() {
        let temp = tempdir().unwrap();
        make_private(temp.path());
        let trace_directory = temp.path().join("trace-store");
        fs::create_dir(&trace_directory).unwrap();
        fs::set_permissions(&trace_directory, fs::Permissions::from_mode(0o700)).unwrap();

        let file_path = trace_directory.join("file-replacement.ppseg");
        let (mut file_writer, _, _) = setup(&file_path);
        file_writer.append_network(&publish()).unwrap();
        let open_path = open_path_for(&file_path);
        let displaced_file = trace_directory.join("displaced.open");
        fs::rename(&open_path, &displaced_file).unwrap();
        fs::write(&open_path, b"replacement-must-not-change").unwrap();
        fs::set_permissions(&open_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(file_writer.sync_data().is_err());
        assert_eq!(
            fs::read(&open_path).unwrap(),
            b"replacement-must-not-change"
        );
        assert!(!file_path.exists());

        fs::remove_file(&open_path).unwrap();
        fs::remove_file(&displaced_file).unwrap();
        let directory_path = trace_directory.join("directory-replacement.ppseg");
        let (mut directory_writer, _, _) = setup(&directory_path);
        directory_writer.append_network(&publish()).unwrap();
        let displaced_directory = temp.path().join("displaced-trace-store");
        fs::rename(&trace_directory, &displaced_directory).unwrap();
        fs::create_dir(&trace_directory).unwrap();
        fs::set_permissions(&trace_directory, fs::Permissions::from_mode(0o700)).unwrap();
        let sentinel = trace_directory.join("sentinel");
        fs::write(&sentinel, b"replacement-directory").unwrap();
        assert!(directory_writer.sync_data().is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"replacement-directory");
        assert!(!trace_directory
            .join("directory-replacement.ppseg.open")
            .exists());
        assert!(displaced_directory
            .join("directory-replacement.ppseg.open")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_segment_is_rejected_without_blocking() {
        let temp = tempdir().unwrap();
        make_private(temp.path());
        let fifo = temp.path().join("trace.fifo");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(verify_segment(&fifo).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn purpose_and_jurisdiction_are_enforced_before_write() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("trace.ppseg");
        let (mut writer, _, _) = setup(&path);
        let mut record = publish();
        record.jurisdiction = Jurisdiction::Eu;
        assert!(matches!(
            writer.append_network(&record),
            Err(SealError::WrongPurpose)
        ));
        let mut stale = publish();
        stale.timestamp_ms = key_id(CompliancePurpose::NetworkTrace).epoch_start_ms
            + pigeonpost_compliance_format::TRACE_EPOCH_DURATION_MS;
        assert!(matches!(
            writer.append_network(&stale),
            Err(SealError::InvalidRecord)
        ));
        let identity = IdentityTraceRecord {
            jurisdiction: Jurisdiction::Test,
            timestamp_ms: 1,
            node_id: [1; 32],
            correlation_id: [2; 32],
            provider: crate::trace::IdentityProvider::Oidc,
            provider_subject: "subject".into(),
        };
        assert!(matches!(
            writer.append_identity(&identity),
            Err(SealError::WrongPurpose)
        ));
    }
}
