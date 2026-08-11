//! Signed terminal inventory for one closed trace epoch.
//!
//! Segment signatures prove each file independently. This manifest additionally proves that an
//! epoch is terminal and that its ordered segment list is complete. Local paths are transport
//! metadata and deliberately never enter the signed representation.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(all(test, unix))]
use std::fs;
#[cfg(not(unix))]
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::io::{Seek, SeekFrom};

use ed25519_dalek::{Signature, VerifyingKey};
use pigeonpost_compliance_format::{
    trace_epoch_contains, trace_epoch_end_ms, ComplianceKeyId, CompliancePurpose,
    COMPLIANCE_KEY_ID_LEN,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, OpenAccess,
};
use rand_core::{OsRng, RngCore};

use crate::error::{Result, SealError};
#[cfg(not(unix))]
use crate::segment::{rename_no_replace, secure_artifact_parent, sync_parent};
use crate::segment::{
    require_persistent_writer_platform, PersistentWriterPlatform, SegmentSigner, VerifiedSegment,
    MAX_SEGMENT_RECORDS,
};

const MANIFEST_MAGIC: &[u8; 8] = b"PPEPOCH\0";
/// Exact version of the terminal epoch manifest.
pub const EPOCH_MANIFEST_VERSION: u8 = 1;
const MANIFEST_SIGNATURE_DOMAIN: &[u8] = b"pigeonpost/trace-epoch-manifest/v1";
const MANIFEST_HEADER_LEN: usize = 204;
/// Exact encoded length of one manifest segment entry.
pub const EPOCH_MANIFEST_ENTRY_LEN: usize = 120;
const MANIFEST_SIGNATURE_LEN: usize = 64;
/// Fixed encoded bytes outside the segment-entry list.
pub const EPOCH_MANIFEST_FIXED_LEN: usize = MANIFEST_HEADER_LEN + MANIFEST_SIGNATURE_LEN;
/// Explicit high bound: at 10,000 records per segment this covers 655,360,000 records per epoch.
pub const MAX_EPOCH_MANIFEST_SEGMENTS: u32 = 65_536;
/// Hard byte bound checked before allocating for an untrusted manifest.
pub const MAX_EPOCH_MANIFEST_BYTES: u64 = (EPOCH_MANIFEST_FIXED_LEN
    + MAX_EPOCH_MANIFEST_SEGMENTS as usize * EPOCH_MANIFEST_ENTRY_LEN)
    as u64;

/// One authenticated member of an epoch's exact ordered segment inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochSegmentEntry {
    segment_index: u32,
    segment_id: [u8; 32],
    header_hash: [u8; 32],
    footer_hash: [u8; 32],
    record_count: u32,
    opened_at_ms: u64,
    closed_at_ms: u64,
    // Context retained by entries constructed from verified segments. These values are represented
    // once in the manifest header and let the signing constructor reject mixed inputs.
    key_id: ComplianceKeyId,
    signer_public_key: [u8; 32],
    custody_key_digest: [u8; 32],
    epoch_key_commitment: [u8; 32],
}

impl EpochSegmentEntry {
    /// Derive one manifest entry from a segment whose frame chain and footer already verify.
    pub fn from_verified(segment_index: u32, segment: &VerifiedSegment) -> Result<Self> {
        let entry = Self {
            segment_index,
            segment_id: segment.header.segment_id(),
            header_hash: segment.header.hash(),
            footer_hash: segment.footer.hash(),
            record_count: segment.footer.record_count(),
            opened_at_ms: segment.header.opened_at_ms(),
            closed_at_ms: segment.footer.closed_at_ms(),
            key_id: segment.header.key_id(),
            signer_public_key: segment.header.signer_public_key(),
            custody_key_digest: segment.header.wrapped_epoch_key().compliance_key_digest(),
            epoch_key_commitment: segment.header.wrapped_epoch_key().epoch_key_commitment(),
        };
        entry.validate()?;
        Ok(entry)
    }

    pub const fn segment_index(&self) -> u32 {
        self.segment_index
    }

    pub const fn segment_id(&self) -> [u8; 32] {
        self.segment_id
    }

    pub const fn header_hash(&self) -> [u8; 32] {
        self.header_hash
    }

    pub const fn footer_hash(&self) -> [u8; 32] {
        self.footer_hash
    }

    pub const fn record_count(&self) -> u32 {
        self.record_count
    }

    pub const fn opened_at_ms(&self) -> u64 {
        self.opened_at_ms
    }

    pub const fn closed_at_ms(&self) -> u64 {
        self.closed_at_ms
    }

    fn validate(&self) -> Result<()> {
        if self.segment_id == [0u8; 32]
            || self.header_hash == [0u8; 32]
            || self.footer_hash == [0u8; 32]
            || self.record_count > MAX_SEGMENT_RECORDS
            || self.opened_at_ms == 0
            || self.closed_at_ms < self.opened_at_ms
            || trace_epoch_contains(&self.key_id, self.opened_at_ms) != Ok(true)
            || trace_epoch_contains(&self.key_id, self.closed_at_ms) != Ok(true)
            || self.signer_public_key == [0u8; 32]
            || self.custody_key_digest == [0u8; 32]
            || self.epoch_key_commitment == [0u8; 32]
        {
            return Err(SealError::CorruptManifest);
        }
        Ok(())
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.segment_index.to_be_bytes());
        out.extend_from_slice(&self.segment_id);
        out.extend_from_slice(&self.header_hash);
        out.extend_from_slice(&self.footer_hash);
        out.extend_from_slice(&self.record_count.to_be_bytes());
        out.extend_from_slice(&self.opened_at_ms.to_be_bytes());
        out.extend_from_slice(&self.closed_at_ms.to_be_bytes());
    }

    fn decode(bytes: &[u8], context: ManifestContext) -> Result<Self> {
        if bytes.len() != EPOCH_MANIFEST_ENTRY_LEN {
            return Err(SealError::CorruptManifest);
        }
        let mut segment_id = [0u8; 32];
        segment_id.copy_from_slice(&bytes[4..36]);
        let mut header_hash = [0u8; 32];
        header_hash.copy_from_slice(&bytes[36..68]);
        let mut footer_hash = [0u8; 32];
        footer_hash.copy_from_slice(&bytes[68..100]);
        let entry = Self {
            segment_index: u32::from_be_bytes(bytes[..4].try_into().expect("fixed slice")),
            segment_id,
            header_hash,
            footer_hash,
            record_count: u32::from_be_bytes(bytes[100..104].try_into().expect("fixed slice")),
            opened_at_ms: u64::from_be_bytes(bytes[104..112].try_into().expect("fixed slice")),
            closed_at_ms: u64::from_be_bytes(bytes[112..120].try_into().expect("fixed slice")),
            key_id: context.key_id,
            signer_public_key: context.signer_public_key,
            custody_key_digest: context.custody_key_digest,
            epoch_key_commitment: context.epoch_key_commitment,
        };
        entry.validate()?;
        Ok(entry)
    }
}

#[derive(Debug, Clone, Copy)]
struct ManifestContext {
    key_id: ComplianceKeyId,
    signer_public_key: [u8; 32],
    custody_key_digest: [u8; 32],
    epoch_key_commitment: [u8; 32],
}

/// Producer-signed terminal marker for exactly one closed trace epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochManifest {
    key_id: ComplianceKeyId,
    producer_node_id: [u8; 32],
    signer_public_key: [u8; 32],
    custody_key_digest: [u8; 32],
    epoch_key_commitment: [u8; 32],
    epoch_end_ms: u64,
    total_records: u64,
    segments: Vec<EpochSegmentEntry>,
    signature: [u8; 64],
}

impl EpochManifest {
    /// Build and sign the canonical terminal marker for an exact verified segment list.
    pub fn new_signed(
        key_id: ComplianceKeyId,
        producer_node_id: [u8; 32],
        custody_key_digest: [u8; 32],
        epoch_key_commitment: [u8; 32],
        segments: Vec<EpochSegmentEntry>,
        signer: &impl SegmentSigner,
    ) -> Result<Self> {
        let epoch_end_ms = trace_epoch_end_ms(&key_id).map_err(|_| SealError::CorruptManifest)?;
        let total_records = segments.iter().try_fold(0u64, |total, segment| {
            total
                .checked_add(u64::from(segment.record_count))
                .ok_or(SealError::LimitExceeded)
        })?;
        let mut manifest = Self {
            key_id,
            producer_node_id,
            signer_public_key: signer.verifying_key(),
            custody_key_digest,
            epoch_key_commitment,
            epoch_end_ms,
            total_records,
            segments,
            signature: [0u8; 64],
        };
        manifest.validate_structure()?;
        manifest.signature = signer.sign(&manifest.signature_preimage()?);
        manifest.verify_signature()?;
        Ok(manifest)
    }

    pub const fn key_id(&self) -> ComplianceKeyId {
        self.key_id
    }

    pub const fn producer_node_id(&self) -> [u8; 32] {
        self.producer_node_id
    }

    pub const fn signer_public_key(&self) -> [u8; 32] {
        self.signer_public_key
    }

    pub const fn custody_key_digest(&self) -> [u8; 32] {
        self.custody_key_digest
    }

    pub const fn epoch_key_commitment(&self) -> [u8; 32] {
        self.epoch_key_commitment
    }

    pub const fn epoch_end_ms(&self) -> u64 {
        self.epoch_end_ms
    }

    pub fn total_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).expect("bounded manifest")
    }

    pub const fn total_records(&self) -> u64 {
        self.total_records
    }

    pub fn segments(&self) -> &[EpochSegmentEntry] {
        &self.segments
    }

    pub const fn signature(&self) -> [u8; 64] {
        self.signature
    }

    /// Encode and reverify the one canonical signed representation.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate_structure()?;
        self.verify_signature()?;
        let mut out = self.encode_unsigned()?;
        out.extend_from_slice(&self.signature);
        Ok(out)
    }

    /// Decode and verify an exact canonical representation using its embedded producer key.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < EPOCH_MANIFEST_FIXED_LEN
            || bytes.len() as u64 > MAX_EPOCH_MANIFEST_BYTES
            || &bytes[..8] != MANIFEST_MAGIC
            || bytes[8] != EPOCH_MANIFEST_VERSION
        {
            return Err(SealError::CorruptManifest);
        }
        let key_id = ComplianceKeyId::decode(&bytes[9..9 + COMPLIANCE_KEY_ID_LEN])
            .map_err(|_| SealError::CorruptManifest)?;
        let mut producer_node_id = [0u8; 32];
        producer_node_id.copy_from_slice(&bytes[56..88]);
        let mut signer_public_key = [0u8; 32];
        signer_public_key.copy_from_slice(&bytes[88..120]);
        let mut custody_key_digest = [0u8; 32];
        custody_key_digest.copy_from_slice(&bytes[120..152]);
        let mut epoch_key_commitment = [0u8; 32];
        epoch_key_commitment.copy_from_slice(&bytes[152..184]);
        let epoch_end_ms = u64::from_be_bytes(bytes[184..192].try_into().expect("fixed slice"));
        let total_segments = u32::from_be_bytes(bytes[192..196].try_into().expect("fixed slice"));
        let total_records = u64::from_be_bytes(bytes[196..204].try_into().expect("fixed slice"));
        if total_segments > MAX_EPOCH_MANIFEST_SEGMENTS {
            return Err(SealError::LimitExceeded);
        }
        let entries_len = usize::try_from(total_segments)
            .map_err(|_| SealError::LimitExceeded)?
            .checked_mul(EPOCH_MANIFEST_ENTRY_LEN)
            .ok_or(SealError::LimitExceeded)?;
        let expected_len = EPOCH_MANIFEST_FIXED_LEN
            .checked_add(entries_len)
            .ok_or(SealError::LimitExceeded)?;
        if bytes.len() != expected_len {
            return Err(SealError::CorruptManifest);
        }
        let context = ManifestContext {
            key_id,
            signer_public_key,
            custody_key_digest,
            epoch_key_commitment,
        };
        let mut segments = Vec::with_capacity(total_segments as usize);
        let mut cursor = MANIFEST_HEADER_LEN;
        for _ in 0..total_segments {
            segments.push(EpochSegmentEntry::decode(
                &bytes[cursor..cursor + EPOCH_MANIFEST_ENTRY_LEN],
                context,
            )?);
            cursor += EPOCH_MANIFEST_ENTRY_LEN;
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[cursor..]);
        let manifest = Self {
            key_id,
            producer_node_id,
            signer_public_key,
            custody_key_digest,
            epoch_key_commitment,
            epoch_end_ms,
            total_records,
            segments,
            signature,
        };
        manifest.validate_structure()?;
        manifest.verify_signature()?;
        Ok(manifest)
    }

    /// Decode and additionally pin the trusted producer signer.
    pub fn decode_for_signer(bytes: &[u8], expected_signer: [u8; 32]) -> Result<Self> {
        let manifest = Self::decode(bytes)?;
        if manifest.signer_public_key != expected_signer {
            return Err(SealError::BadSignature);
        }
        Ok(manifest)
    }

    /// Start a streaming completeness check. [`EpochManifestVerifier::finish`] is mandatory.
    pub fn verifier(&self) -> EpochManifestVerifier<'_> {
        EpochManifestVerifier {
            manifest: self,
            next_index: 0,
            verified_records: 0,
        }
    }

    fn validate_structure(&self) -> Result<()> {
        let expected_end =
            trace_epoch_end_ms(&self.key_id).map_err(|_| SealError::CorruptManifest)?;
        if !matches!(
            self.key_id.purpose,
            CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace
        ) || self.producer_node_id == [0u8; 32]
            || self.signer_public_key == [0u8; 32]
            || self.custody_key_digest == [0u8; 32]
            || self.epoch_key_commitment == [0u8; 32]
            || self.epoch_end_ms != expected_end
            || self.segments.len() > MAX_EPOCH_MANIFEST_SEGMENTS as usize
        {
            return Err(SealError::CorruptManifest);
        }
        parse_verifying_key(&self.signer_public_key)?;
        let mut ids = HashSet::with_capacity(self.segments.len());
        let mut records = 0u64;
        for (position, segment) in self.segments.iter().enumerate() {
            segment.validate()?;
            if segment.segment_index
                != u32::try_from(position).map_err(|_| SealError::LimitExceeded)?
                || segment.key_id != self.key_id
                || segment.signer_public_key != self.signer_public_key
                || segment.custody_key_digest != self.custody_key_digest
                || segment.epoch_key_commitment != self.epoch_key_commitment
                || !ids.insert(segment.segment_id)
            {
                return Err(SealError::CorruptManifest);
            }
            records = records
                .checked_add(u64::from(segment.record_count))
                .ok_or(SealError::LimitExceeded)?;
        }
        if records != self.total_records {
            return Err(SealError::CorruptManifest);
        }
        Ok(())
    }

    fn encode_unsigned(&self) -> Result<Vec<u8>> {
        let entries_len = self
            .segments
            .len()
            .checked_mul(EPOCH_MANIFEST_ENTRY_LEN)
            .ok_or(SealError::LimitExceeded)?;
        let mut out = Vec::with_capacity(MANIFEST_HEADER_LEN + entries_len);
        out.extend_from_slice(MANIFEST_MAGIC);
        out.push(EPOCH_MANIFEST_VERSION);
        out.extend_from_slice(
            &self
                .key_id
                .encode()
                .map_err(|_| SealError::CorruptManifest)?,
        );
        out.extend_from_slice(&self.producer_node_id);
        out.extend_from_slice(&self.signer_public_key);
        out.extend_from_slice(&self.custody_key_digest);
        out.extend_from_slice(&self.epoch_key_commitment);
        out.extend_from_slice(&self.epoch_end_ms.to_be_bytes());
        out.extend_from_slice(&self.total_segments().to_be_bytes());
        out.extend_from_slice(&self.total_records.to_be_bytes());
        for segment in &self.segments {
            segment.encode(&mut out);
        }
        debug_assert_eq!(out.len(), MANIFEST_HEADER_LEN + entries_len);
        Ok(out)
    }

    fn signature_preimage(&self) -> Result<Vec<u8>> {
        let unsigned = self.encode_unsigned()?;
        let mut preimage = Vec::with_capacity(MANIFEST_SIGNATURE_DOMAIN.len() + unsigned.len());
        preimage.extend_from_slice(MANIFEST_SIGNATURE_DOMAIN);
        preimage.extend_from_slice(&unsigned);
        Ok(preimage)
    }

    fn verify_signature(&self) -> Result<()> {
        parse_verifying_key(&self.signer_public_key)?
            .verify_strict(
                &self.signature_preimage()?,
                &Signature::from_bytes(&self.signature),
            )
            .map_err(|_| SealError::BadSignature)
    }
}

/// Stateful verifier that detects omission and extra segments without retaining segment bodies.
#[derive(Debug)]
pub struct EpochManifestVerifier<'a> {
    manifest: &'a EpochManifest,
    next_index: usize,
    verified_records: u64,
}

impl EpochManifestVerifier<'_> {
    pub fn verify_next(&mut self, segment: &VerifiedSegment) -> Result<()> {
        let entry = self
            .manifest
            .segments
            .get(self.next_index)
            .ok_or(SealError::CorruptManifest)?;
        if segment.header.key_id() != self.manifest.key_id
            || segment.header.signer_public_key() != self.manifest.signer_public_key
            || segment.header.wrapped_epoch_key().compliance_key_digest()
                != self.manifest.custody_key_digest
            || segment.header.wrapped_epoch_key().epoch_key_commitment()
                != self.manifest.epoch_key_commitment
            || segment.header.segment_id() != entry.segment_id
            || segment.header.hash() != entry.header_hash
            || segment.footer.hash() != entry.footer_hash
            || segment.footer.record_count() != entry.record_count
            || segment.header.opened_at_ms() != entry.opened_at_ms
            || segment.footer.closed_at_ms() != entry.closed_at_ms
        {
            return Err(SealError::CorruptManifest);
        }
        self.verified_records = self
            .verified_records
            .checked_add(u64::from(segment.footer.record_count()))
            .ok_or(SealError::LimitExceeded)?;
        self.next_index += 1;
        Ok(())
    }

    /// Prove that every signed entry, and no extra entry, was supplied in order.
    pub fn finish(self) -> Result<()> {
        if self.next_index != self.manifest.segments.len()
            || self.verified_records != self.manifest.total_records
        {
            return Err(SealError::CorruptManifest);
        }
        Ok(())
    }
}

/// Stable local filename for a manifest. The resulting path is not part of its signature.
pub fn epoch_manifest_path(
    directory: impl AsRef<Path>,
    key_id: &ComplianceKeyId,
) -> Result<PathBuf> {
    let purpose = match key_id.purpose {
        CompliancePurpose::NetworkTrace => "network",
        CompliancePurpose::IdentityTrace => "identity",
        CompliancePurpose::Attribution => return Err(SealError::WrongPurpose),
    };
    trace_epoch_end_ms(key_id).map_err(|_| SealError::CorruptManifest)?;
    let encoded = key_id.encode().map_err(|_| SealError::CorruptManifest)?;
    Ok(directory
        .as_ref()
        .join(format!("{purpose}-{}.ppmanifest", hex(&encoded))))
}

/// Structurally verify one bounded manifest while rejecting unsafe path shape, replacement, size
/// changes, and bad signatures where the platform exposes those facts. This portable reader does
/// not establish the offline owner-only custody contract; offline consumers use the platform-gated
/// authenticated trace-epoch API for that stronger guarantee.
pub fn read_epoch_manifest(path: impl AsRef<Path>) -> Result<EpochManifest> {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        read_manifest_guard(open_manifest_file(path)?)
    }
    #[cfg(not(unix))]
    {
        let file = open_manifest_file(path)?;
        let metadata = file.metadata()?;
        if metadata.len() > MAX_EPOCH_MANIFEST_BYTES {
            return Err(SealError::LimitExceeded);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&file)
            .take(MAX_EPOCH_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != metadata.len()
            || bytes.len() as u64 > MAX_EPOCH_MANIFEST_BYTES
            || !named_manifest_matches(&file, path)
        {
            return Err(SealError::CorruptManifest);
        }
        EpochManifest::decode(&bytes)
    }
}

/// Read one manifest and additionally pin the trusted producer signer.
pub fn read_epoch_manifest_for_signer(
    path: impl AsRef<Path>,
    expected_signer: [u8; 32],
) -> Result<EpochManifest> {
    let manifest = read_epoch_manifest(path)?;
    if manifest.signer_public_key != expected_signer {
        return Err(SealError::BadSignature);
    }
    Ok(manifest)
}

/// Atomically publish one owner-only terminal marker, accepting only an identical prior result.
pub fn publish_epoch_manifest(path: impl AsRef<Path>, manifest: &EpochManifest) -> Result<()> {
    publish_epoch_manifest_for_platform(PersistentWriterPlatform::current(), path, manifest)
}

fn publish_epoch_manifest_for_platform(
    platform: PersistentWriterPlatform,
    path: impl AsRef<Path>,
    manifest: &EpochManifest,
) -> Result<()> {
    require_persistent_writer_platform(platform)?;
    let path = path.as_ref();
    let bytes = manifest.encode()?;
    #[cfg(unix)]
    {
        publish_epoch_manifest_unix(path, manifest, &bytes)
    }
    #[cfg(not(unix))]
    {
        if path.exists() {
            return identical_existing(path, manifest);
        }
        let parent = path.parent().ok_or_else(invalid_manifest_path)?;
        secure_artifact_parent(parent)?;
        let (temp, mut file) = create_temp(parent)?;
        let write_result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok::<(), SealError>(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        match rename_no_replace(&temp, path) {
            Ok(()) => {
                if !named_manifest_matches(&file, path) {
                    return Err(SealError::CorruptManifest);
                }
                sync_parent(parent)?;
                identical_existing(path, manifest)
            }
            Err(SealError::AlreadyExists) => {
                let _ = fs::remove_file(&temp);
                identical_existing(path, manifest)
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                Err(error)
            }
        }
    }
}

fn invalid_manifest_path() -> SealError {
    SealError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "manifest path must have a parent and leaf name",
    ))
}

#[cfg(unix)]
fn manifest_leaf(path: &Path) -> Result<LeafName> {
    LeafName::new(path.file_name().ok_or_else(invalid_manifest_path)?)
        .map_err(map_manifest_custody_error)
}

#[cfg(unix)]
fn publish_epoch_manifest_unix(path: &Path, manifest: &EpochManifest, bytes: &[u8]) -> Result<()> {
    let parent_path = path.parent().ok_or_else(invalid_manifest_path)?;
    let directory = GuardedDir::create_private(parent_path).map_err(map_manifest_custody_error)?;
    let destination = manifest_leaf(path)?;
    if let Some(existing) = directory
        .open_file_optional(
            &destination,
            OpenAccess::ReadOnly,
            FilePolicy::private(MAX_EPOCH_MANIFEST_BYTES),
        )
        .map_err(map_manifest_custody_error)?
    {
        return identical_existing_guard(existing, manifest);
    }

    let (temporary_name, mut temporary) = create_temp(&directory)?;
    if let Err(error) = temporary.write_all(bytes) {
        let _ = directory.unlink_file(temporary);
        return Err(SealError::Io(error));
    }
    let cleanup = directory
        .open_file(
            &temporary_name,
            OpenAccess::ReadWrite,
            FilePolicy::private(MAX_EPOCH_MANIFEST_BYTES),
        )
        .map_err(map_manifest_custody_error)?;
    match directory.publish_no_replace(temporary, &directory, &destination) {
        Ok(published) => identical_existing_guard(published, manifest),
        Err(CustodyError::AlreadyExists) => {
            directory
                .unlink_file(cleanup)
                .map_err(map_manifest_custody_error)?;
            let existing = directory
                .open_file(
                    &destination,
                    OpenAccess::ReadOnly,
                    FilePolicy::private(MAX_EPOCH_MANIFEST_BYTES),
                )
                .map_err(map_manifest_custody_error)?;
            identical_existing_guard(existing, manifest)
        }
        Err(error) => {
            let _ = directory.unlink_file(cleanup);
            Err(map_manifest_custody_error(error))
        }
    }
}

#[cfg(unix)]
fn identical_existing_guard(file: GuardedFile, expected: &EpochManifest) -> Result<()> {
    if read_manifest_guard(file)? == *expected {
        Ok(())
    } else {
        Err(SealError::AlreadyExists)
    }
}

#[cfg(unix)]
fn read_manifest_guard(mut file: GuardedFile) -> Result<EpochManifest> {
    let metadata = file.metadata().map_err(map_manifest_custody_error)?;
    if metadata.len > MAX_EPOCH_MANIFEST_BYTES {
        return Err(SealError::LimitExceeded);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(metadata.len as usize);
    (&mut file)
        .take(MAX_EPOCH_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len || bytes.len() as u64 > MAX_EPOCH_MANIFEST_BYTES {
        return Err(SealError::CorruptManifest);
    }
    file.verify_named().map_err(map_manifest_custody_error)?;
    EpochManifest::decode(&bytes)
}

#[cfg(unix)]
fn create_temp(parent: &GuardedDir) -> Result<(LeafName, GuardedFile)> {
    for _ in 0..16 {
        let mut random = [0u8; 16];
        OsRng.fill_bytes(&mut random);
        let name = LeafName::new(format!(
            ".ppmanifest.{}.{}.tmp",
            std::process::id(),
            hex(&random)
        ))
        .map_err(map_manifest_custody_error)?;
        match parent.create_file(&name, FilePolicy::private(MAX_EPOCH_MANIFEST_BYTES)) {
            Ok(file) => return Ok((name, file)),
            Err(CustodyError::AlreadyExists) => continue,
            Err(error) => return Err(map_manifest_custody_error(error)),
        }
    }
    Err(SealError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate manifest temporary file",
    )))
}

#[cfg(unix)]
fn map_manifest_custody_error(error: CustodyError) -> SealError {
    match error {
        CustodyError::Io(error) => SealError::Io(error),
        CustodyError::NotFound => SealError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "manifest custody object was not found",
        )),
        CustodyError::AlreadyExists => SealError::AlreadyExists,
        CustodyError::LimitExceeded(_) => SealError::LimitExceeded,
        CustodyError::InvalidPath(_)
        | CustodyError::UnsafeAncestor(_)
        | CustodyError::UnsafeDirectory(_)
        | CustodyError::UnsafeFile(_)
        | CustodyError::Unsupported(_) => SealError::CorruptManifest,
    }
}

#[cfg(not(unix))]
fn identical_existing(path: &Path, expected: &EpochManifest) -> Result<()> {
    if read_epoch_manifest_for_signer(path, expected.signer_public_key)? == *expected {
        Ok(())
    } else {
        Err(SealError::AlreadyExists)
    }
}

#[cfg(not(unix))]
fn create_temp(parent: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..16 {
        let mut random = [0u8; 16];
        OsRng.fill_bytes(&mut random);
        let path = parent.join(format!(
            ".ppmanifest.{}.{}.tmp",
            std::process::id(),
            hex(&random)
        ));
        let mut options = OpenOptions::new();
        options.write(true).read(true).create_new(true);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SealError::Io(error)),
        }
    }
    Err(SealError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate manifest temporary file",
    )))
}

#[cfg(unix)]
fn open_manifest_file(path: &Path) -> Result<GuardedFile> {
    let parent_path = path.parent().ok_or_else(invalid_manifest_path)?;
    let directory = GuardedDir::open_existing(parent_path, DirPolicy::private())
        .map_err(map_manifest_custody_error)?;
    directory
        .open_file(
            &manifest_leaf(path)?,
            OpenAccess::ReadOnly,
            FilePolicy::private(MAX_EPOCH_MANIFEST_BYTES),
        )
        .map_err(map_manifest_custody_error)
}

#[cfg(not(unix))]
fn open_manifest_file(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SealError::CorruptManifest);
    }
    File::open(path).map_err(SealError::Io)
}

#[cfg(not(unix))]
fn named_manifest_matches(file: &File, path: &Path) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

fn parse_verifying_key(bytes: &[u8; 32]) -> Result<VerifyingKey> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| SealError::InvalidKey)?;
    if key.is_weak() {
        return Err(SealError::InvalidKey);
    }
    Ok(key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
    use pigeonpost_compliance_format::{Jurisdiction, TRACE_EPOCH_DURATION_MS};

    use super::*;
    use crate::{EpochSealingKey, NetworkOperation, SegmentWriter, TraceIp, TraceRecord};

    const EPOCH_START: u64 = 20_000 * TRACE_EPOCH_DURATION_MS;

    struct PanicPath;

    impl AsRef<Path> for PanicPath {
        fn as_ref(&self) -> &Path {
            panic!("unsupported manifest publication must not inspect its path")
        }
    }

    #[test]
    fn unsupported_platform_rejects_manifest_publication_before_path_access() {
        let signer = SigningKey::from_bytes(&[9; 32]);
        let manifest = EpochManifest::new_signed(
            key_id(CompliancePurpose::NetworkTrace, 3),
            [4; 32],
            [5; 32],
            [6; 32],
            Vec::new(),
            &signer,
        )
        .unwrap();

        assert!(matches!(
            publish_epoch_manifest_for_platform(
                PersistentWriterPlatform::unsupported_for_test(),
                PanicPath,
                &manifest,
            ),
            Err(SealError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
    }

    fn key_id(purpose: CompliancePurpose, authority: u8) -> ComplianceKeyId {
        ComplianceKeyId::new(purpose, Jurisdiction::Test, [authority; 32], EPOCH_START, 1)
    }

    fn record(timestamp_ms: u64, event: u8) -> TraceRecord {
        TraceRecord {
            jurisdiction: Jurisdiction::Test,
            operation: NetworkOperation::Publish,
            timestamp_ms,
            node_id: [4; 32],
            source_ip: TraceIp::V4(Ipv4Addr::new(192, 0, 2, event)),
            source_port: 4242,
            event_id: Some([event; 32]),
            recipient: Some([5; 32]),
            owner: None,
            size_bytes: 64,
            correlation_id: None,
        }
    }

    fn segment(
        directory: &Path,
        key_id: ComplianceKeyId,
        secret: [u8; 32],
        signer: &SigningKey,
        index: u32,
        opened_at_ms: u64,
    ) -> VerifiedSegment {
        #[cfg(unix)]
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        let custody_public = MontgomeryPoint::mul_base_clamped([7; 32]).to_bytes();
        let path = directory.join(format!("segment-{index}.pptrace"));
        let mut writer = SegmentWriter::create(
            &path,
            EpochSealingKey::from_bytes(key_id, secret).unwrap(),
            &custody_public,
            signer.verifying_key().to_bytes(),
            opened_at_ms,
            10,
        )
        .unwrap();
        writer
            .append_network(&record(opened_at_ms + 1, index as u8 + 1))
            .unwrap();
        writer.finalize(opened_at_ms + 2, signer).unwrap()
    }

    fn fixture(directory: &Path) -> (SigningKey, Vec<VerifiedSegment>, EpochManifest) {
        let signer = SigningKey::from_bytes(&[9; 32]);
        let key_id = key_id(CompliancePurpose::NetworkTrace, 3);
        let first = segment(directory, key_id, [8; 32], &signer, 0, EPOCH_START + 10);
        let second = segment(directory, key_id, [8; 32], &signer, 1, EPOCH_START + 20);
        let entries = vec![
            EpochSegmentEntry::from_verified(0, &first).unwrap(),
            EpochSegmentEntry::from_verified(1, &second).unwrap(),
        ];
        let manifest = EpochManifest::new_signed(
            key_id,
            [4; 32],
            first.header.wrapped_epoch_key().compliance_key_digest(),
            first.header.wrapped_epoch_key().epoch_key_commitment(),
            entries,
            &signer,
        )
        .unwrap();
        (signer, vec![first, second], manifest)
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn canonical_manifest_streaming_verifier_detects_omission_and_extra_input() {
        let root = tempfile::tempdir().unwrap();
        let (signer, segments, manifest) = fixture(root.path());
        let bytes = manifest.encode().unwrap();
        let decoded =
            EpochManifest::decode_for_signer(&bytes, signer.verifying_key().to_bytes()).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(decoded.total_segments(), 2);
        assert_eq!(decoded.total_records(), 2);
        assert_eq!(
            decoded.epoch_end_ms(),
            EPOCH_START + TRACE_EPOCH_DURATION_MS
        );

        let mut complete = decoded.verifier();
        complete.verify_next(&segments[0]).unwrap();
        complete.verify_next(&segments[1]).unwrap();
        complete.finish().unwrap();

        let mut omitted = decoded.verifier();
        omitted.verify_next(&segments[0]).unwrap();
        assert!(omitted.finish().is_err());

        let mut extra = decoded.verifier();
        extra.verify_next(&segments[0]).unwrap();
        extra.verify_next(&segments[1]).unwrap();
        assert!(extra.verify_next(&segments[1]).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn reorder_duplicate_and_mixed_epoch_inputs_are_rejected_before_signing() {
        let root = tempfile::tempdir().unwrap();
        let (signer, segments, manifest) = fixture(root.path());

        let reordered = vec![
            EpochSegmentEntry::from_verified(1, &segments[1]).unwrap(),
            EpochSegmentEntry::from_verified(0, &segments[0]).unwrap(),
        ];
        assert!(EpochManifest::new_signed(
            manifest.key_id(),
            manifest.producer_node_id(),
            manifest.custody_key_digest(),
            manifest.epoch_key_commitment(),
            reordered,
            &signer,
        )
        .is_err());

        let first = EpochSegmentEntry::from_verified(0, &segments[0]).unwrap();
        let mut duplicate = first.clone();
        duplicate.segment_index = 1;
        assert!(EpochManifest::new_signed(
            manifest.key_id(),
            manifest.producer_node_id(),
            manifest.custody_key_digest(),
            manifest.epoch_key_commitment(),
            vec![first, duplicate],
            &signer,
        )
        .is_err());

        let mixed = segment(
            root.path(),
            key_id(CompliancePurpose::NetworkTrace, 6),
            [8; 32],
            &signer,
            2,
            EPOCH_START + 30,
        );
        assert!(EpochManifest::new_signed(
            manifest.key_id(),
            manifest.producer_node_id(),
            manifest.custody_key_digest(),
            manifest.epoch_key_commitment(),
            vec![EpochSegmentEntry::from_verified(0, &mixed).unwrap()],
            &signer,
        )
        .is_err());
        let mut verifier = manifest.verifier();
        assert!(verifier.verify_next(&mixed).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn decoder_rejects_tamper_wrong_signer_and_noncanonical_terminal_marker() {
        let root = tempfile::tempdir().unwrap();
        let (signer, _, manifest) = fixture(root.path());
        let bytes = manifest.encode().unwrap();
        let wrong = SigningKey::from_bytes(&[10; 32]);
        assert!(
            EpochManifest::decode_for_signer(&bytes, wrong.verifying_key().to_bytes()).is_err()
        );

        for offset in [9, 56, 120, 152, 184, 196, 204, bytes.len() - 1] {
            let mut tampered = bytes.clone();
            tampered[offset] ^= 1;
            assert!(EpochManifest::decode(&tampered).is_err(), "offset {offset}");
        }
        assert!(EpochManifest::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut extended = bytes.clone();
        extended.push(0);
        assert!(EpochManifest::decode(&extended).is_err());

        let mut invalid_terminal = manifest.clone();
        invalid_terminal.epoch_end_ms += 1;
        invalid_terminal.signature = signer.sign(&invalid_terminal.signature_preimage().unwrap());
        let mut encoded = invalid_terminal.encode_unsigned().unwrap();
        encoded.extend_from_slice(&invalid_terminal.signature);
        assert!(EpochManifest::decode(&encoded).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn publication_is_owner_only_atomic_idempotent_and_path_independent() {
        let root = tempfile::tempdir().unwrap();
        let (_, _, manifest) = fixture(root.path());
        let first = epoch_manifest_path(root.path(), &manifest.key_id()).unwrap();
        let second = root.path().join("transport-alias.ppmanifest");
        publish_epoch_manifest(&first, &manifest).unwrap();
        publish_epoch_manifest(&first, &manifest).unwrap();
        publish_epoch_manifest(&second, &manifest).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        assert_eq!(read_epoch_manifest(&first).unwrap(), manifest);

        let other_root = tempfile::tempdir().unwrap();
        let (_, _, conflicting) = fixture(other_root.path());
        let original = fs::read(&first).unwrap();
        assert!(matches!(
            publish_epoch_manifest(&first, &conflicting),
            Err(SealError::AlreadyExists)
        ));
        assert_eq!(fs::read(&first).unwrap(), original);
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ppmanifest.")
        }));

        #[cfg(unix)]
        {
            assert_eq!(fs::metadata(first).unwrap().permissions().mode() & 0o077, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn manifest_paths_reject_hardlinks_symlinks_and_fifos() {
        let root = tempfile::tempdir().unwrap();
        let (_, _, manifest) = fixture(root.path());
        let path = epoch_manifest_path(root.path(), &manifest.key_id()).unwrap();
        publish_epoch_manifest(&path, &manifest).unwrap();

        let alias = root.path().join("manifest-hardlink.ppmanifest");
        fs::hard_link(&path, &alias).unwrap();
        assert!(read_epoch_manifest(&path).is_err());
        fs::remove_file(&alias).unwrap();

        let link = root.path().join("manifest-symlink.ppmanifest");
        symlink(&path, &link).unwrap();
        assert!(read_epoch_manifest(&link).is_err());

        let fifo = root.path().join("manifest.fifo");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(read_epoch_manifest(&fifo).is_err());
    }
}
