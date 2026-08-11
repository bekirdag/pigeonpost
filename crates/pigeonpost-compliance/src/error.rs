use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ComplianceError {
    #[error("offline compliance operations are unsupported on this platform")]
    UnsupportedPlatform,
    #[error("invalid custody key")]
    InvalidKey,
    #[error("wrong compliance purpose or jurisdiction")]
    WrongPurpose,
    #[error("disclosure is not authorized")]
    Unauthorized,
    #[error("approval signature or approver is invalid")]
    BadApproval,
    #[error("disclosure request is invalid")]
    InvalidRequest,
    #[error("disclosure request expired")]
    Expired,
    #[error("operation conflicts with current state")]
    StateConflict,
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("attribution block is invalid")]
    AttributionInvalid,
    #[error("trace segment is invalid")]
    SegmentInvalid,
    #[error("retention period is still active")]
    RetentionActive,
    #[error("an active legal hold blocks destruction")]
    LegalHoldActive,
    #[error("the destruction inventory is incomplete")]
    IncompleteInventory,
    #[error("unknown inventory copy")]
    UnknownCopy,
    #[error("disclosure log reached its bound")]
    LimitExceeded,
    #[error("compliance storage operation failed")]
    Storage,
}

pub type Result<T> = core::result::Result<T, ComplianceError>;
