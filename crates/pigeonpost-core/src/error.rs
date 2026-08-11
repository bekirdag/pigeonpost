use thiserror::Error;

/// Every way a core operation can fail.
///
/// Variants deliberately carry no attacker-controlled data beyond what the caller already
/// holds: error text reaches logs, and mail content must never reach a log line (`sds.md` §9).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("malformed address: {0}")]
    MalformedAddress(&'static str),

    #[error("invalid base32")]
    InvalidBase32,

    #[error("invalid key material")]
    InvalidKey,

    #[error("signature verification failed")]
    BadSignature,

    #[error("decryption failed")]
    DecryptionFailed,

    #[error("malformed envelope: {0}")]
    MalformedEnvelope(&'static str),

    #[error("sender in sealed message does not match the sealing key")]
    SenderMismatch,

    #[error("successor commitment mismatch: rotation target was not the committed key")]
    SuccessorMismatch,

    #[error("sequence number did not increase (replay)")]
    StaleSequence,

    #[error("proof-of-work below required difficulty")]
    InsufficientWork,

    #[error("message exceeds the maximum size")]
    TooLarge,

    #[error("timestamp outside the accepted window")]
    StaleTimestamp,
}

pub type Result<T> = core::result::Result<T, Error>;
