use thiserror::Error;

#[derive(Debug, Error)]
pub enum SealError {
    #[error("I/O operation failed")]
    Io(#[from] std::io::Error),
    #[error("invalid compliance format")]
    Format,
    #[error("invalid trace record")]
    InvalidRecord,
    #[error("wrong compliance purpose")]
    WrongPurpose,
    #[error("invalid key material")]
    InvalidKey,
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("segment reached its record bound")]
    SegmentFull,
    #[error("segment is corrupt or not canonically encoded")]
    CorruptSegment,
    #[error("epoch manifest is corrupt or not canonically encoded")]
    CorruptManifest,
    #[error("segment signature verification failed")]
    BadSignature,
    #[error("segment path already exists")]
    AlreadyExists,
    #[error("segment exceeds verification limits")]
    LimitExceeded,
    #[error("trace writer lease is held or unavailable")]
    WriterLeaseUnavailable,
    #[error("trace writer lease artifact is unsafe")]
    UnsafeWriterLease,
    #[error("trace storage budget is exhausted or invalid")]
    StorageLimit,
}

pub type Result<T> = core::result::Result<T, SealError>;
