//! Online-safe compliance primitives.
//!
//! This package can encode and seal trace records, wrap short-lived epoch keys to a public custody
//! key, write crash-recoverable hash-chained segments, and verify closed segments publicly. It has
//! deliberately no decrypt/unseal API and no compliance custody secret type.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod error;
mod key;
mod manifest;
mod segment;
mod storage_budget;
mod trace;
mod writer_lease;

pub use error::{Result, SealError};
pub use key::{
    trace_key_wrap_salt, EpochSealingKey, WrappedEpochKey, TRACE_KEY_WRAP_HKDF_INFO,
    TRACE_KEY_WRAP_VERSION, WRAPPED_EPOCH_KEY_LEN,
};
pub use manifest::{
    epoch_manifest_path, publish_epoch_manifest, read_epoch_manifest,
    read_epoch_manifest_for_signer, EpochManifest, EpochManifestVerifier, EpochSegmentEntry,
    EPOCH_MANIFEST_ENTRY_LEN, EPOCH_MANIFEST_FIXED_LEN, EPOCH_MANIFEST_VERSION,
    MAX_EPOCH_MANIFEST_BYTES, MAX_EPOCH_MANIFEST_SEGMENTS,
};
pub use pigeonpost_compliance_format::TRACE_EPOCH_DURATION_MS;
pub use segment::{
    recover_segment, verify_owner_only_segment, verify_segment, Recovery, SealedFrame,
    SegmentFooter, SegmentHeader, SegmentSigner, SegmentWriter, VerifiedSegment, MAX_SEGMENT_BYTES,
    MAX_SEGMENT_RECORDS, SEGMENT_FOOTER_LEN, SEGMENT_HEADER_LEN,
};
pub use storage_budget::{
    required_identity_trace_storage_bytes, required_network_trace_storage_bytes,
    TraceStorageBudget, DEFAULT_TRACE_STORAGE_BYTES, IDENTITY_TRACE_FRAME_BYTES,
    MAX_TRACE_STORAGE_BYTES, MIN_TRACE_STORAGE_BYTES, NETWORK_TRACE_FRAME_BYTES,
    TRACE_LIVE_KEY_BYTES, TRACE_RATE_WINDOWS_PER_UTC_DAY, TRACE_TERMINAL_RESERVE_BYTES,
};
pub use trace::{
    IdentityProvider, IdentityTraceRecord, NetworkOperation, TraceIp, TraceRecord,
    IDENTITY_TRACE_RECORD_LEN, TRACE_RECORD_LEN,
};
pub use writer_lease::{TraceWriterLease, TRACE_WRITER_LEASE_NAME};
