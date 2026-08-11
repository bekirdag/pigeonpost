//! Exact, bounded offline consumption of one producer-closed trace epoch.

#[cfg(not(unix))]
use std::fs::{self, File};
#[cfg(unix)]
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
#[cfg(any(not(unix), test))]
use std::path::PathBuf;

#[cfg(unix)]
use ed25519_dalek::{Signature, VerifyingKey};
use pigeonpost_compliance_format::{trace_epoch_end_ms, ComplianceKeyId, CompliancePurpose};
#[cfg(not(unix))]
use pigeonpost_compliance_seal::{
    epoch_manifest_path, read_epoch_manifest_for_signer, verify_owner_only_segment,
};
use pigeonpost_compliance_seal::{EpochManifest, VerifiedSegment};
#[cfg(unix)]
use pigeonpost_compliance_seal::{
    SealedFrame, SegmentFooter, SegmentHeader, IDENTITY_TRACE_RECORD_LEN, MAX_EPOCH_MANIFEST_BYTES,
    MAX_EPOCH_MANIFEST_SEGMENTS, MAX_SEGMENT_BYTES, MAX_SEGMENT_RECORDS, SEGMENT_FOOTER_LEN,
    SEGMENT_HEADER_LEN, TRACE_RECORD_LEN,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    DirPolicy, DirectoryEntry, EntryKind, FilePolicy, GuardedDir, GuardedFile, LeafName,
    ObjectIdentity, OpenAccess,
};
use sha2::{Digest, Sha256};

use crate::error::{ComplianceError, Result};

const MANIFEST_COMMITMENT_DOMAIN: &[u8] = b"pigeonpost/offline-terminal-manifest-commitment/v1";

/// Independently pinned facts required before an offline terminal manifest is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceEpochExpectation {
    pub key_id: ComplianceKeyId,
    pub producer_node_id: [u8; 32],
    pub signer_public_key: [u8; 32],
    pub custody_key_digest: [u8; 32],
    /// Offline observation time. It must be at or after the canonical exclusive epoch end.
    pub observed_at_ms: u64,
}

/// One authenticated terminal bundle. It retains only the bounded manifest and a directory guard;
/// segment bodies are verified and released one at a time.
pub struct AuthenticatedTraceEpoch {
    #[cfg(not(unix))]
    directory: PathBuf,
    #[cfg(not(unix))]
    directory_guard: File,
    #[cfg(unix)]
    directory_guard: GuardedDir,
    #[cfg(unix)]
    manifest_guard: GuardedFile,
    #[cfg(unix)]
    initial_entries: Vec<DirectoryEntry>,
    #[cfg(unix)]
    segment_identities: Option<Vec<ObjectIdentity>>,
    manifest: EpochManifest,
    manifest_commitment: [u8; 32],
}

/// Result of checking the ciphertext bodies named by an already authenticated terminal manifest.
///
/// Destruction treats `Degraded` as evidence to preserve, not as a reason to retain the epoch key:
/// ciphertext loss or corruption must never make its decryption capability immortal. Disclosure
/// continues to use [`AuthenticatedTraceEpoch::open`] and [`AuthenticatedTraceEpoch::verify_all`],
/// both of which fail closed unless the complete declared bundle verifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceBodyIntegrity {
    Verified,
    Degraded,
}

impl core::fmt::Debug for AuthenticatedTraceEpoch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AuthenticatedTraceEpoch")
            .field("key_id", &self.manifest.key_id())
            .field("total_segments", &self.manifest.total_segments())
            .field("total_records", &self.manifest.total_records())
            .field("directory", &"<withheld>")
            .finish()
    }
}

impl AuthenticatedTraceEpoch {
    /// Open a dedicated owner-only bundle directory containing exactly one manifest and its exact
    /// declared segment set. No caller-supplied segment paths are accepted.
    pub fn open(directory: impl AsRef<Path>, expected: TraceEpochExpectation) -> Result<Self> {
        Self::open_for_platform(
            crate::platform::OfflinePlatform::current(),
            directory,
            expected,
        )
    }

    fn open_for_platform(
        platform: crate::platform::OfflinePlatform,
        directory: impl AsRef<Path>,
        expected: TraceEpochExpectation,
    ) -> Result<Self> {
        platform.require()?;
        #[cfg(unix)]
        {
            let mut epoch = Self::open_authenticated_manifest(directory, expected)?;
            epoch.segment_identities = Some(epoch.validate_initial_layout()?);
            Ok(epoch)
        }
        #[cfg(not(unix))]
        {
            let epoch = Self::open_authenticated_manifest(directory, expected)?;
            epoch.verify_exact_layout()?;
            Ok(epoch)
        }
    }

    /// Authenticate only the terminal manifest for the destruction path.
    ///
    /// This deliberately remains crate-private. The signed manifest, pinned producer, signer,
    /// custody digest, key id, canonical epoch end, and stable owner-only directory remain hard
    /// requirements. Only the ciphertext body/layout check is deferred so missing or corrupt
    /// bodies cannot prevent destruction after retention and holds have cleared.
    pub(crate) fn open_for_destruction(
        directory: impl AsRef<Path>,
        expected: TraceEpochExpectation,
    ) -> Result<Self> {
        Self::open_for_destruction_on_platform(
            crate::platform::OfflinePlatform::current(),
            directory,
            expected,
        )
    }

    fn open_for_destruction_on_platform(
        platform: crate::platform::OfflinePlatform,
        directory: impl AsRef<Path>,
        expected: TraceEpochExpectation,
    ) -> Result<Self> {
        platform.require()?;
        Self::open_authenticated_manifest(directory, expected)
    }

    fn open_authenticated_manifest(
        directory: impl AsRef<Path>,
        expected: TraceEpochExpectation,
    ) -> Result<Self> {
        validate_expectation(&expected)?;
        let directory = directory.as_ref();

        #[cfg(unix)]
        let (directory_guard, manifest_guard, initial_entries, manifest) = {
            let directory_guard = GuardedDir::open_existing(directory, DirPolicy::private())
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            let manifest_name = manifest_leaf_name(expected.key_id)?;
            let manifest_guard = directory_guard
                .open_file(
                    &manifest_name,
                    OpenAccess::ReadOnly,
                    FilePolicy::private(MAX_EPOCH_MANIFEST_BYTES),
                )
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            let bytes = read_guarded_file(&manifest_guard, MAX_EPOCH_MANIFEST_BYTES)?;
            let manifest = EpochManifest::decode_for_signer(&bytes, expected.signer_public_key)
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            manifest_guard
                .verify_named()
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            let limit = usize::try_from(MAX_EPOCH_MANIFEST_SEGMENTS)
                .map_err(|_| ComplianceError::LimitExceeded)?
                .checked_add(2)
                .ok_or(ComplianceError::LimitExceeded)?;
            let initial_entries = list_directory_bounded(&directory_guard, limit)?;
            (directory_guard, manifest_guard, initial_entries, manifest)
        };

        #[cfg(not(unix))]
        let (directory_guard, manifest) = {
            let directory_guard = open_private_directory(directory)?;
            let manifest_path = epoch_manifest_path(directory, &expected.key_id)
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            let manifest =
                read_epoch_manifest_for_signer(&manifest_path, expected.signer_public_key)
                    .map_err(|_| ComplianceError::SegmentInvalid)?;
            (directory_guard, manifest)
        };
        if manifest.key_id() != expected.key_id
            || manifest.producer_node_id() != expected.producer_node_id
            || manifest.signer_public_key() != expected.signer_public_key
            || manifest.custody_key_digest() != expected.custody_key_digest
            || manifest.epoch_end_ms()
                != trace_epoch_end_ms(&expected.key_id)
                    .map_err(|_| ComplianceError::SegmentInvalid)?
        {
            return Err(ComplianceError::SegmentInvalid);
        }
        let manifest_commitment = commit_manifest(&manifest)?;
        Ok(Self {
            #[cfg(not(unix))]
            directory: directory.to_path_buf(),
            directory_guard,
            #[cfg(unix)]
            manifest_guard,
            #[cfg(unix)]
            initial_entries,
            #[cfg(unix)]
            segment_identities: None,
            manifest,
            manifest_commitment,
        })
    }

    pub const fn key_id(&self) -> ComplianceKeyId {
        self.manifest.key_id()
    }

    pub const fn producer_node_id(&self) -> [u8; 32] {
        self.manifest.producer_node_id()
    }

    pub const fn epoch_key_commitment(&self) -> [u8; 32] {
        self.manifest.epoch_key_commitment()
    }

    pub const fn manifest_commitment(&self) -> [u8; 32] {
        self.manifest_commitment
    }

    pub fn total_segments(&self) -> u32 {
        self.manifest.total_segments()
    }

    pub const fn total_records(&self) -> u64 {
        self.manifest.total_records()
    }

    /// Verify the complete terminal inventory without retaining segment bodies.
    pub fn verify_all(&self) -> Result<()> {
        self.for_each_segment(|_, _| Ok(()))
    }

    /// Assess ciphertext bodies after authenticating the terminal manifest.
    ///
    /// Manifest replacement, directory replacement, or loss of the authenticated terminal marker
    /// remains a hard error. Any failure confined to the manifest-declared ciphertext set is
    /// reported as `Degraded`, allowing the caller to persist that fact before destroying keys.
    pub(crate) fn assess_body_integrity(&self) -> Result<TraceBodyIntegrity> {
        self.revalidate_manifest()?;
        #[cfg(unix)]
        self.verify_initial_inventory()?;
        #[cfg(unix)]
        let verified = self
            .validate_initial_layout()
            .and_then(|identities| self.verify_segments_against(&identities))
            .is_ok();
        #[cfg(not(unix))]
        let verified = self.verify_all().is_ok();
        let integrity = if verified {
            TraceBodyIntegrity::Verified
        } else {
            TraceBodyIntegrity::Degraded
        };
        #[cfg(unix)]
        self.verify_initial_inventory()?;
        self.revalidate_manifest()?;
        Ok(integrity)
    }

    /// Revalidate every retained bundle name before a later custody side effect.
    #[cfg(unix)]
    pub(crate) fn verify_custody(&self) -> Result<()> {
        self.revalidate_manifest()?;
        self.verify_initial_inventory()
    }

    /// Verify and consume every segment in signed order, one at a time.
    ///
    /// The callback may decrypt/select records from the current segment. If it fails, public
    /// verification continues through `finish()` before its error is returned, so no successful
    /// caller can accidentally accept only a prefix of the signed inventory.
    pub fn for_each_segment(
        &self,
        mut consume: impl FnMut(u32, &VerifiedSegment) -> Result<()>,
    ) -> Result<()> {
        self.revalidate_manifest()?;
        #[cfg(unix)]
        self.verify_pinned_layout()?;
        #[cfg(not(unix))]
        self.verify_exact_layout()?;
        let mut verifier = self.manifest.verifier();
        let mut consume_error = None;
        for index in 0..self.manifest.total_segments() {
            #[cfg(unix)]
            let segment = self.open_and_verify_segment(index)?;
            #[cfg(not(unix))]
            let segment = {
                let path = self.segment_path(index)?;
                verify_owner_only_segment(&path).map_err(|_| ComplianceError::SegmentInvalid)?
            };
            verifier
                .verify_next(&segment)
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            if consume_error.is_none() {
                if let Err(error) = consume(index, &segment) {
                    consume_error = Some(error);
                }
            }
        }
        verifier
            .finish()
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        #[cfg(unix)]
        self.verify_pinned_layout()?;
        #[cfg(not(unix))]
        self.verify_exact_layout()?;
        self.revalidate_manifest()?;
        if let Some(error) = consume_error {
            return Err(error);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn segment_path(&self, index: u32) -> Result<PathBuf> {
        if index >= self.manifest.total_segments() {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(self
            .directory
            .join(segment_file_name(self.key_id(), index)?))
    }

    #[cfg(not(unix))]
    fn revalidate_manifest(&self) -> Result<()> {
        assert_directory_stable(&self.directory_guard, &self.directory)?;
        let path = epoch_manifest_path(&self.directory, &self.key_id())
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let current = read_epoch_manifest_for_signer(path, self.manifest.signer_public_key())
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        if current != self.manifest || commit_manifest(&current)? != self.manifest_commitment {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn verify_exact_layout(&self) -> Result<()> {
        assert_directory_stable(&self.directory_guard, &self.directory)?;
        let manifest_path = epoch_manifest_path(&self.directory, &self.key_id())
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let manifest_name = manifest_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ComplianceError::SegmentInvalid)?;
        let expected_entries = usize::try_from(self.manifest.total_segments())
            .map_err(|_| ComplianceError::LimitExceeded)?
            .checked_add(1)
            .ok_or(ComplianceError::LimitExceeded)?;
        let mut entries = 0usize;
        let mut manifest_seen = false;
        let mut segments_seen = 0u32;
        for entry in fs::read_dir(&self.directory).map_err(|_| ComplianceError::Storage)? {
            let entry = entry.map_err(|_| ComplianceError::Storage)?;
            entries = entries
                .checked_add(1)
                .filter(|count| *count <= expected_entries)
                .ok_or(ComplianceError::SegmentInvalid)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            if name == manifest_name {
                if manifest_seen {
                    return Err(ComplianceError::SegmentInvalid);
                }
                manifest_seen = true;
                continue;
            }
            let index = parse_segment_file_name(self.key_id(), &name)?;
            if index >= self.manifest.total_segments() {
                return Err(ComplianceError::SegmentInvalid);
            }
            segments_seen = segments_seen
                .checked_add(1)
                .ok_or(ComplianceError::LimitExceeded)?;
        }
        assert_directory_stable(&self.directory_guard, &self.directory)?;
        if entries != expected_entries
            || !manifest_seen
            || segments_seen != self.manifest.total_segments()
        {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn revalidate_manifest(&self) -> Result<()> {
        self.directory_guard
            .verify_named()
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        self.manifest_guard
            .verify_named()
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let bytes = read_guarded_file(&self.manifest_guard, MAX_EPOCH_MANIFEST_BYTES)?;
        let current = EpochManifest::decode_for_signer(&bytes, self.manifest.signer_public_key())
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        if current != self.manifest || commit_manifest(&current)? != self.manifest_commitment {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn validate_initial_layout(&self) -> Result<Vec<ObjectIdentity>> {
        validate_layout_entries(
            &self.directory_guard,
            &self.initial_entries,
            &self.manifest_guard,
            &self.manifest,
        )
    }

    #[cfg(unix)]
    fn verify_initial_inventory(&self) -> Result<()> {
        self.directory_guard
            .verify_named()
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let current = list_directory_bounded(
            &self.directory_guard,
            self.initial_entries
                .len()
                .checked_add(1)
                .ok_or(ComplianceError::LimitExceeded)?,
        )?;
        if current != self.initial_entries {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn verify_pinned_layout(&self) -> Result<()> {
        self.verify_initial_inventory()?;
        let current = validate_layout_entries(
            &self.directory_guard,
            &self.initial_entries,
            &self.manifest_guard,
            &self.manifest,
        )?;
        if self.segment_identities.as_deref() != Some(current.as_slice()) {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn open_and_verify_segment(&self, index: u32) -> Result<VerifiedSegment> {
        let expected_identity = self
            .segment_identities
            .as_ref()
            .and_then(|identities| identities.get(index as usize))
            .ok_or(ComplianceError::SegmentInvalid)?;
        self.open_and_verify_segment_with_identity(index, expected_identity)
    }

    #[cfg(unix)]
    fn open_and_verify_segment_with_identity(
        &self,
        index: u32,
        expected_identity: &ObjectIdentity,
    ) -> Result<VerifiedSegment> {
        let name = LeafName::new(segment_file_name(self.key_id(), index)?)
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let guard = self
            .directory_guard
            .open_file(
                &name,
                OpenAccess::ReadOnly,
                FilePolicy::private(MAX_SEGMENT_BYTES),
            )
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        if guard.identity() != *expected_identity {
            return Err(ComplianceError::SegmentInvalid);
        }
        let bytes = read_guarded_file(&guard, MAX_SEGMENT_BYTES)?;
        let segment = verify_segment_bytes(&bytes)?;
        guard
            .verify_named()
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        if guard.identity() != *expected_identity {
            return Err(ComplianceError::SegmentInvalid);
        }
        Ok(segment)
    }

    #[cfg(unix)]
    fn verify_segments_against(&self, identities: &[ObjectIdentity]) -> Result<()> {
        if identities.len() != self.manifest.total_segments() as usize {
            return Err(ComplianceError::SegmentInvalid);
        }
        let mut verifier = self.manifest.verifier();
        for (index, identity) in identities.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| ComplianceError::LimitExceeded)?;
            let segment = self.open_and_verify_segment_with_identity(index, identity)?;
            verifier
                .verify_next(&segment)
                .map_err(|_| ComplianceError::SegmentInvalid)?;
        }
        verifier
            .finish()
            .map_err(|_| ComplianceError::SegmentInvalid)
    }
}

#[cfg(unix)]
fn manifest_leaf_name(key_id: ComplianceKeyId) -> Result<LeafName> {
    let purpose = purpose_name(key_id.purpose)?;
    trace_epoch_end_ms(&key_id).map_err(|_| ComplianceError::SegmentInvalid)?;
    let encoded = key_id
        .encode()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    LeafName::new(format!("{purpose}-{}.ppmanifest", hex(&encoded)))
        .map_err(|_| ComplianceError::SegmentInvalid)
}

#[cfg(unix)]
fn read_guarded_file(guard: &GuardedFile, maximum: u64) -> Result<Vec<u8>> {
    guard
        .verify_named()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    let metadata = guard
        .metadata()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    if metadata.len > maximum {
        return Err(ComplianceError::LimitExceeded);
    }
    let mut file = guard
        .file()
        .try_clone()
        .map_err(|_| ComplianceError::Storage)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ComplianceError::Storage)?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len).map_err(|_| ComplianceError::LimitExceeded)?,
    );
    file.take(
        maximum
            .checked_add(1)
            .ok_or(ComplianceError::LimitExceeded)?,
    )
    .read_to_end(&mut bytes)
    .map_err(|_| ComplianceError::Storage)?;
    guard
        .verify_named()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    let final_metadata = guard
        .metadata()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    if bytes.len() as u64 != metadata.len || final_metadata != metadata {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn list_directory_bounded(directory: &GuardedDir, limit: usize) -> Result<Vec<DirectoryEntry>> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    directory
        .verify_named()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    let mut reader = rustix::fs::Dir::read_from(directory).map_err(|_| ComplianceError::Storage)?;
    let mut entries = Vec::new();
    for raw in &mut reader {
        let raw = raw.map_err(|_| ComplianceError::Storage)?;
        let bytes = raw.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if entries.len() == limit {
            return Err(ComplianceError::LimitExceeded);
        }
        let name = LeafName::new(OsString::from_vec(bytes.to_vec()))
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        let metadata = directory
            .entry_metadata(&name)
            .map_err(|_| ComplianceError::Storage)?
            .ok_or(ComplianceError::SegmentInvalid)?;
        entries.push(DirectoryEntry { name, metadata });
    }
    directory
        .verify_named()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[cfg(unix)]
fn validate_layout_entries(
    directory: &GuardedDir,
    entries: &[DirectoryEntry],
    manifest_guard: &GuardedFile,
    manifest: &EpochManifest,
) -> Result<Vec<ObjectIdentity>> {
    let expected_entries = usize::try_from(manifest.total_segments())
        .map_err(|_| ComplianceError::LimitExceeded)?
        .checked_add(1)
        .ok_or(ComplianceError::LimitExceeded)?;
    if entries.len() != expected_entries {
        return Err(ComplianceError::SegmentInvalid);
    }
    let manifest_name = manifest_leaf_name(manifest.key_id())?;
    let mut manifest_seen = false;
    let mut identities = vec![None; manifest.total_segments() as usize];
    for entry in entries {
        if entry.metadata.kind != EntryKind::RegularFile {
            return Err(ComplianceError::SegmentInvalid);
        }
        if entry.name == manifest_name {
            if manifest_seen || entry.metadata.identity != manifest_guard.identity() {
                return Err(ComplianceError::SegmentInvalid);
            }
            manifest_guard
                .verify_named()
                .map_err(|_| ComplianceError::SegmentInvalid)?;
            manifest_seen = true;
            continue;
        }
        let name = entry
            .name
            .as_os_str()
            .to_str()
            .ok_or(ComplianceError::SegmentInvalid)?;
        let index = parse_segment_file_name(manifest.key_id(), name)?;
        if index >= manifest.total_segments() {
            return Err(ComplianceError::SegmentInvalid);
        }
        let guard = directory
            .open_file(
                &entry.name,
                OpenAccess::ReadOnly,
                FilePolicy::private(MAX_SEGMENT_BYTES),
            )
            .map_err(|_| ComplianceError::SegmentInvalid)?;
        if guard.identity() != entry.metadata.identity
            || identities[index as usize]
                .replace(guard.identity())
                .is_some()
        {
            return Err(ComplianceError::SegmentInvalid);
        }
        guard
            .verify_named()
            .map_err(|_| ComplianceError::SegmentInvalid)?;
    }
    if !manifest_seen || identities.iter().any(Option::is_none) {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(identities
        .into_iter()
        .map(|identity| identity.expect("checked complete segment identity inventory"))
        .collect())
}

#[cfg(unix)]
fn verify_segment_bytes(bytes: &[u8]) -> Result<VerifiedSegment> {
    const FRAME_CHAIN_DOMAIN: &[u8] = b"pigeonpost/trace-frame-chain/v1";
    const FOOTER_SIGNATURE_DOMAIN: &[u8] = b"pigeonpost/trace-segment-footer/v1";

    if bytes.len() < SEGMENT_HEADER_LEN + SEGMENT_FOOTER_LEN
        || bytes.len() as u64 > MAX_SEGMENT_BYTES
    {
        return Err(ComplianceError::SegmentInvalid);
    }
    let header = SegmentHeader::decode(&bytes[..SEGMENT_HEADER_LEN])
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    let footer = SegmentFooter::decode(&bytes[bytes.len() - SEGMENT_FOOTER_LEN..])
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    if footer.record_count() > header.max_records() || footer.record_count() > MAX_SEGMENT_RECORDS {
        return Err(ComplianceError::LimitExceeded);
    }
    let plaintext_len = match header.key_id().purpose {
        CompliancePurpose::NetworkTrace => TRACE_RECORD_LEN,
        CompliancePurpose::IdentityTrace => IDENTITY_TRACE_RECORD_LEN,
        CompliancePurpose::Attribution => return Err(ComplianceError::WrongPurpose),
    };
    let frame_len = 8usize
        .checked_add(24)
        .and_then(|value| value.checked_add(plaintext_len))
        .and_then(|value| value.checked_add(16 + 32))
        .ok_or(ComplianceError::LimitExceeded)?;
    let expected_len = SEGMENT_HEADER_LEN
        .checked_add(
            (footer.record_count() as usize)
                .checked_mul(frame_len)
                .ok_or(ComplianceError::LimitExceeded)?,
        )
        .and_then(|length| length.checked_add(SEGMENT_FOOTER_LEN))
        .ok_or(ComplianceError::LimitExceeded)?;
    if bytes.len() != expected_len || footer.closed_at_ms() < header.opened_at_ms() {
        return Err(ComplianceError::SegmentInvalid);
    }

    let mut frames = Vec::with_capacity(footer.record_count() as usize);
    let mut previous_hash = header.hash();
    let mut offset = SEGMENT_HEADER_LEN;
    for sequence in 0..footer.record_count() {
        let frame =
            SealedFrame::decode(&bytes[offset..offset + frame_len], header.key_id().purpose)
                .map_err(|_| ComplianceError::SegmentInvalid)?;
        let mut hash = Sha256::new();
        hash.update(FRAME_CHAIN_DOMAIN);
        hash.update(frame.sequence().to_be_bytes());
        hash.update(previous_hash);
        hash.update(frame.nonce());
        hash.update(frame.ciphertext());
        let chain_hash: [u8; 32] = hash.finalize().into();
        if frame.sequence() != u64::from(sequence) || frame.chain_hash() != chain_hash {
            return Err(ComplianceError::SegmentInvalid);
        }
        previous_hash = frame.chain_hash();
        frames.push(frame);
        offset += frame_len;
    }
    let first = frames.first().map_or([0u8; 32], SealedFrame::chain_hash);
    if first != footer.first_record_hash() || previous_hash != footer.final_chain_hash() {
        return Err(ComplianceError::SegmentInvalid);
    }
    let verifying_key = VerifyingKey::from_bytes(&header.signer_public_key())
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    if verifying_key.is_weak() {
        return Err(ComplianceError::SegmentInvalid);
    }
    let mut preimage = Vec::with_capacity(FOOTER_SIGNATURE_DOMAIN.len() + 32 + 4 + 32 + 32 + 8);
    preimage.extend_from_slice(FOOTER_SIGNATURE_DOMAIN);
    preimage.extend_from_slice(&header.hash());
    preimage.extend_from_slice(&footer.record_count().to_be_bytes());
    preimage.extend_from_slice(&footer.first_record_hash());
    preimage.extend_from_slice(&footer.final_chain_hash());
    preimage.extend_from_slice(&footer.closed_at_ms().to_be_bytes());
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&footer.signature()))
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    Ok(VerifiedSegment {
        header,
        frames,
        footer,
    })
}

fn validate_expectation(expected: &TraceEpochExpectation) -> Result<()> {
    let epoch_end =
        trace_epoch_end_ms(&expected.key_id).map_err(|_| ComplianceError::SegmentInvalid)?;
    if !matches!(
        expected.key_id.purpose,
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace
    ) || expected.producer_node_id == [0u8; 32]
        || expected.signer_public_key == [0u8; 32]
        || expected.custody_key_digest == [0u8; 32]
        || expected.observed_at_ms < epoch_end
    {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(())
}

fn commit_manifest(manifest: &EpochManifest) -> Result<[u8; 32]> {
    let encoded = manifest
        .encode()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    let mut hash = Sha256::new();
    hash.update(MANIFEST_COMMITMENT_DOMAIN);
    hash.update((encoded.len() as u64).to_be_bytes());
    hash.update(encoded);
    let commitment: [u8; 32] = hash.finalize().into();
    if commitment == [0u8; 32] {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(commitment)
}

fn segment_file_name(key_id: ComplianceKeyId, index: u32) -> Result<String> {
    let purpose = purpose_name(key_id.purpose)?;
    let encoded = key_id
        .encode()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    Ok(format!("{purpose}-{}-{index:08}.pptrace", hex(&encoded)))
}

fn parse_segment_file_name(key_id: ComplianceKeyId, name: &str) -> Result<u32> {
    let purpose = purpose_name(key_id.purpose)?;
    let encoded = key_id
        .encode()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    let prefix = format!("{purpose}-{}-", hex(&encoded));
    let index = name
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(".pptrace"))
        .filter(|value| value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(ComplianceError::SegmentInvalid)?
        .parse::<u32>()
        .map_err(|_| ComplianceError::SegmentInvalid)?;
    if segment_file_name(key_id, index)? != name {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(index)
}

fn purpose_name(purpose: CompliancePurpose) -> Result<&'static str> {
    match purpose {
        CompliancePurpose::NetworkTrace => Ok("network"),
        CompliancePurpose::IdentityTrace => Ok("identity"),
        CompliancePurpose::Attribution => Err(ComplianceError::WrongPurpose),
    }
}

#[cfg(not(unix))]
fn open_private_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() || fs::canonicalize(path).map_err(|_| ComplianceError::Storage)? != path
    {
        return Err(ComplianceError::SegmentInvalid);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ComplianceError::Storage)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ComplianceError::SegmentInvalid);
    }
    File::open(path).map_err(|_| ComplianceError::Storage)
}

#[cfg(not(unix))]
fn assert_directory_stable(_file: &File, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ComplianceError::Storage)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ComplianceError::SegmentInvalid);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::Ipv4Addr;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use curve25519_dalek::montgomery::MontgomeryPoint;
    use ed25519_dalek::SigningKey;
    use pigeonpost_compliance_format::{Jurisdiction, TRACE_EPOCH_DURATION_MS};
    use pigeonpost_compliance_seal::{
        epoch_manifest_path, publish_epoch_manifest, EpochManifest, EpochSealingKey,
        EpochSegmentEntry, NetworkOperation, SegmentWriter, TraceIp, TraceRecord,
    };

    use super::*;

    const EPOCH_START: u64 = 20_000 * TRACE_EPOCH_DURATION_MS;

    struct PanicPath;

    impl AsRef<Path> for PanicPath {
        fn as_ref(&self) -> &Path {
            panic!("unsupported trace-epoch operation must not inspect its path")
        }
    }

    #[test]
    fn unsupported_platform_rejects_trace_epoch_paths_before_inspection() {
        let platform = crate::platform::OfflinePlatform::unsupported_for_test();
        let expected = TraceEpochExpectation {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [1; 32],
                EPOCH_START,
                1,
            ),
            producer_node_id: [2; 32],
            signer_public_key: [3; 32],
            custody_key_digest: [4; 32],
            observed_at_ms: EPOCH_START + TRACE_EPOCH_DURATION_MS,
        };

        assert!(matches!(
            AuthenticatedTraceEpoch::open_for_platform(platform, PanicPath, expected),
            Err(ComplianceError::UnsupportedPlatform)
        ));
        assert!(matches!(
            AuthenticatedTraceEpoch::open_for_destruction_on_platform(
                platform, PanicPath, expected
            ),
            Err(ComplianceError::UnsupportedPlatform)
        ));
    }

    struct Fixture {
        directory: tempfile::TempDir,
        key_id: ComplianceKeyId,
        expectation: TraceEpochExpectation,
    }

    impl Fixture {
        fn path(&self) -> PathBuf {
            fs::canonicalize(self.directory.path()).unwrap()
        }
    }

    fn record(timestamp_ms: u64, event: u8) -> TraceRecord {
        TraceRecord {
            jurisdiction: Jurisdiction::Test,
            operation: NetworkOperation::Publish,
            timestamp_ms,
            node_id: [4; 32],
            source_ip: TraceIp::V4(Ipv4Addr::new(192, 0, 2, event)),
            source_port: 4_000 + u16::from(event),
            event_id: Some([event; 32]),
            recipient: Some([5; 32]),
            owner: None,
            size_bytes: 64,
            correlation_id: None,
        }
    }

    fn fixture(segment_count: u32) -> Fixture {
        fixture_with_authority(segment_count, [3; 32], [8; 32], [9; 32])
    }

    fn fixture_with_authority(
        segment_count: u32,
        authority: [u8; 32],
        secret: [u8; 32],
        signer_seed: [u8; 32],
    ) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            authority,
            EPOCH_START,
            1,
        );
        let signer = SigningKey::from_bytes(&signer_seed);
        let custody_public = MontgomeryPoint::mul_base_clamped([7; 32]).to_bytes();
        let mut verified = Vec::new();
        for index in 0..segment_count {
            let path = directory
                .path()
                .join(segment_file_name(key_id, index).unwrap());
            let opened_at_ms = EPOCH_START + 10 + u64::from(index) * 3;
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
            verified.push(writer.finalize(opened_at_ms + 2, &signer).unwrap());
        }
        let entries = verified
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                EpochSegmentEntry::from_verified(index as u32, segment).unwrap()
            })
            .collect();
        let custody_key_digest: [u8; 32] = Sha256::digest(custody_public).into();
        let epoch_key_commitment: [u8; 32] = Sha256::digest(secret).into();
        let manifest = EpochManifest::new_signed(
            key_id,
            [4; 32],
            custody_key_digest,
            epoch_key_commitment,
            entries,
            &signer,
        )
        .unwrap();
        publish_epoch_manifest(
            epoch_manifest_path(directory.path(), &key_id).unwrap(),
            &manifest,
        )
        .unwrap();
        Fixture {
            expectation: TraceEpochExpectation {
                key_id,
                producer_node_id: [4; 32],
                signer_public_key: signer.verifying_key().to_bytes(),
                custody_key_digest,
                observed_at_ms: EPOCH_START + TRACE_EPOCH_DURATION_MS,
            },
            directory,
            key_id,
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn exact_bundle_streams_more_than_the_old_sixty_four_file_ceiling() {
        let fixture = fixture(65);
        let epoch = AuthenticatedTraceEpoch::open(fixture.path(), fixture.expectation).unwrap();
        assert_eq!(epoch.total_segments(), 65);
        assert_eq!(epoch.total_records(), 65);
        assert_ne!(epoch.manifest_commitment(), [0; 32]);
        assert_ne!(epoch.epoch_key_commitment(), [0; 32]);
        let mut next = 0u32;
        epoch
            .for_each_segment(|index, segment| {
                assert_eq!(index, next);
                assert_eq!(segment.footer.record_count(), 1);
                next += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(next, 65);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn omission_extra_reorder_duplicate_and_mixed_inputs_fail_closed() {
        let omitted = fixture(2);
        fs::remove_file(
            omitted
                .directory
                .path()
                .join(segment_file_name(omitted.key_id, 1).unwrap()),
        )
        .unwrap();
        assert!(AuthenticatedTraceEpoch::open(omitted.path(), omitted.expectation).is_err());
        let destruction =
            AuthenticatedTraceEpoch::open_for_destruction(omitted.path(), omitted.expectation)
                .unwrap();
        assert_eq!(
            destruction.assess_body_integrity().unwrap(),
            TraceBodyIntegrity::Degraded
        );

        let extra = fixture(2);
        fs::write(extra.directory.path().join("unexpected"), b"x").unwrap();
        assert!(AuthenticatedTraceEpoch::open(extra.path(), extra.expectation).is_err());

        let reordered = fixture(2);
        let first = reordered
            .directory
            .path()
            .join(segment_file_name(reordered.key_id, 0).unwrap());
        let second = reordered
            .directory
            .path()
            .join(segment_file_name(reordered.key_id, 1).unwrap());
        let swap = reordered.directory.path().join("swap");
        fs::rename(&first, &swap).unwrap();
        fs::rename(&second, &first).unwrap();
        fs::rename(&swap, &second).unwrap();
        let epoch = AuthenticatedTraceEpoch::open(reordered.path(), reordered.expectation).unwrap();
        assert!(epoch.verify_all().is_err());
        assert_eq!(
            epoch.assess_body_integrity().unwrap(),
            TraceBodyIntegrity::Degraded
        );

        let duplicate = fixture(2);
        let first = duplicate
            .directory
            .path()
            .join(segment_file_name(duplicate.key_id, 0).unwrap());
        let second = duplicate
            .directory
            .path()
            .join(segment_file_name(duplicate.key_id, 1).unwrap());
        fs::copy(first, &second).unwrap();
        set_owner_only(&second);
        let epoch = AuthenticatedTraceEpoch::open(duplicate.path(), duplicate.expectation).unwrap();
        assert!(epoch.verify_all().is_err());

        let mixed = fixture(2);
        let other = fixture_with_authority(1, [6; 32], [11; 32], [12; 32]);
        let replacement = mixed
            .directory
            .path()
            .join(segment_file_name(mixed.key_id, 1).unwrap());
        let other_segment = other
            .directory
            .path()
            .join(segment_file_name(other.key_id, 0).unwrap());
        fs::copy(other_segment, &replacement).unwrap();
        set_owner_only(&replacement);
        let epoch = AuthenticatedTraceEpoch::open(mixed.path(), mixed.expectation).unwrap();
        assert!(epoch.verify_all().is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn pins_terminal_metadata_and_rejects_path_aliases() {
        let fixture = fixture(1);
        let mut wrong = fixture.expectation;
        wrong.producer_node_id[0] ^= 1;
        assert!(AuthenticatedTraceEpoch::open(fixture.path(), wrong).is_err());
        let mut wrong = fixture.expectation;
        wrong.custody_key_digest[0] ^= 1;
        assert!(AuthenticatedTraceEpoch::open(fixture.path(), wrong).is_err());
        let mut wrong = fixture.expectation;
        wrong.signer_public_key = SigningKey::from_bytes(&[10; 32]).verifying_key().to_bytes();
        assert!(AuthenticatedTraceEpoch::open(fixture.path(), wrong).is_err());
        let mut early = fixture.expectation;
        early.observed_at_ms -= 1;
        assert!(AuthenticatedTraceEpoch::open(fixture.path(), early).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};

            let alias_root = tempfile::tempdir().unwrap();
            let alias = alias_root.path().join("bundle-alias");
            symlink(fixture.path(), &alias).unwrap();
            assert!(AuthenticatedTraceEpoch::open(alias, fixture.expectation).is_err());

            let path = fixture
                .directory
                .path()
                .join(segment_file_name(fixture.key_id, 0).unwrap());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(AuthenticatedTraceEpoch::open(fixture.path(), fixture.expectation).is_err());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let epoch = AuthenticatedTraceEpoch::open(fixture.path(), fixture.expectation).unwrap();
            let outside = alias_root.path().join("outside-hardlink");
            fs::hard_link(&path, outside).unwrap();
            assert!(epoch.verify_all().is_err());
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn callback_failure_cannot_hide_a_later_inventory_failure() {
        let fixture = fixture(2);
        let epoch = AuthenticatedTraceEpoch::open(fixture.path(), fixture.expectation).unwrap();
        let first = fixture
            .directory
            .path()
            .join(segment_file_name(fixture.key_id, 0).unwrap());
        let second = fixture
            .directory
            .path()
            .join(segment_file_name(fixture.key_id, 1).unwrap());
        fs::copy(first, &second).unwrap();
        set_owner_only(&second);

        let error = epoch
            .for_each_segment(|_, _| Err(ComplianceError::Unauthorized))
            .unwrap_err();
        assert_eq!(error, ComplianceError::SegmentInvalid);
    }

    #[cfg(unix)]
    #[test]
    fn retained_manifest_and_directory_replacement_fail_before_consumption() {
        use std::os::unix::fs::PermissionsExt;

        let manifest_fixture = fixture(1);
        let epoch =
            AuthenticatedTraceEpoch::open(manifest_fixture.path(), manifest_fixture.expectation)
                .unwrap();
        let manifest =
            epoch_manifest_path(manifest_fixture.directory.path(), &manifest_fixture.key_id)
                .unwrap();
        let displaced = manifest.with_extension("displaced");
        fs::rename(&manifest, &displaced).unwrap();
        fs::copy(&displaced, &manifest).unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(epoch.verify_all(), Err(ComplianceError::SegmentInvalid));
        assert!(displaced.exists());

        let directory_fixture = fixture(1);
        let epoch =
            AuthenticatedTraceEpoch::open(directory_fixture.path(), directory_fixture.expectation)
                .unwrap();
        let directory = directory_fixture.path();
        let displaced = directory.with_extension("displaced");
        fs::rename(&directory, &displaced).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(epoch.verify_all(), Err(ComplianceError::SegmentInvalid));
        assert!(displaced
            .join(segment_file_name(directory_fixture.key_id, 0).unwrap())
            .exists());
        fs::remove_dir(&directory).unwrap();
        fs::rename(&displaced, &directory).unwrap();
    }

    fn set_owner_only(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = path;
    }
}
