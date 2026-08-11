//! Offline-only Pigeonpost custody, disclosure, and destruction workflows.
//!
//! This crate is intentionally separate from every online service. Its APIs are being built around
//! explicit authorization and append-only disclosure records; no online crate depends on it.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod approval;
mod audit;
mod custody;
mod error;
mod ledger;
mod operator;
mod platform;
mod retention;
mod trace_epoch;

pub use approval::{
    AuthorizedDisclosure, CommitmentOpenings, DisclosureRequest, DisclosureState,
    SensitiveRequestMaterial,
};
pub use audit::{
    open_private_audit_record, seal_private_audit_record,
    seal_private_audit_record_with_terminal_manifest, EncryptedPrivateAuditRecord, PrivateAuditKey,
    PrivateAuditMaterial, PrivateAuditRecord,
};
pub use custody::{
    disclose_trace_segment, disclose_trace_segment_selected, unseal_attribution,
    AttributionDisclosure, CustodyBackend, DisclosedTraceRecord, SoftwareCustodyKey,
};
pub use error::{ComplianceError, Result};
pub use ledger::{
    CompletionStatus, DisclosureCompletion, DisclosureIntent, DisclosureLeaf, DisclosureLedger,
    DisclosureOutput, MAX_DISCLOSURE_LEAVES,
};
pub use operator::{run_from_env as run_operator_cli, OperatorError};
pub use retention::{
    CopyState, DestructionInventory, InventoryState, KeyCopy, KeyCopyKind, LegalHold,
    RetentionPolicy, TraceIntegrityEvidence, TraceIntegrityStatus, TR_RETENTION_DAYS_MAX,
    TR_RETENTION_DAYS_MIN,
};
pub use trace_epoch::{AuthenticatedTraceEpoch, TraceEpochExpectation};
