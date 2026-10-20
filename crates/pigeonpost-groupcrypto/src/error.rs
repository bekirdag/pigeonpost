//! One error type for the whole API. Messages never contain key material or plaintext.

/// Errors returned by every fallible operation in this crate.
///
/// The variants are deliberately coarse and stable so that the language bindings can map them
/// to a fixed set of codes (see [`Error::code`]).
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    /// A caller-supplied argument is malformed (bad mailbox address, device id, length...).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Bytes from the network or from storage could not be decoded.
    #[error("malformed data: {0}")]
    Decode(&'static str),
    /// The device has no mailbox attestation yet, so it cannot publish key packages or join.
    #[error("device is not attested by its mailbox identity key")]
    NotAttested,
    /// A credential or its mailbox attestation did not verify.
    #[error("invalid credential: {0}")]
    InvalidCredential(String),
    /// No local state for this group.
    #[error("unknown group")]
    UnknownGroup,
    /// A group with this id already exists locally.
    #[error("group already exists")]
    GroupExists,
    /// A commit created by this device is still waiting for the server's verdict.
    #[error("a pending commit must be confirmed or discarded first")]
    PendingCommit,
    /// `confirm_commit`/`discard_commit` was called without a pending commit.
    #[error("no pending commit")]
    NoPendingCommit,
    /// The message belongs to an epoch this device has already left behind.
    #[error("stale epoch: message epoch {message_epoch}, group epoch {group_epoch}")]
    StaleEpoch {
        /// Epoch carried by the message.
        message_epoch: u64,
        /// Current local epoch.
        group_epoch: u64,
    },
    /// The message belongs to a later epoch: earlier handshake messages are missing.
    #[error("future epoch: message epoch {message_epoch}, group epoch {group_epoch}")]
    FutureEpoch {
        /// Epoch carried by the message.
        message_epoch: u64,
        /// Current local epoch.
        group_epoch: u64,
    },
    /// This device was removed from the group (or never completed joining).
    #[error("this device is no longer a member of the group")]
    NotMember,
    /// The same mailbox device would appear twice in the group.
    #[error("duplicate device in group: {0}")]
    DuplicateDevice(String),
    /// The requested member is not in the group.
    #[error("member not found: {0}")]
    MemberNotFound(String),
    /// Authenticated data did not match the authenticated sender or the group.
    #[error("message binding mismatch: {0}")]
    BindingMismatch(&'static str),
    /// Authenticated decryption failed (tampering, wrong key, replay or wrong blob).
    #[error("decryption failed")]
    DecryptionFailed,
    /// The operation is only valid in a group whose members are all this mailbox's own devices.
    #[error("history transfer requires a group of this mailbox's own devices only")]
    NotSelfGroup,
    /// The message kind is not accepted here (for example an unsolicited standalone proposal).
    #[error("unsupported message: {0}")]
    Unsupported(&'static str),
    /// OpenMLS rejected the operation or the message.
    #[error("mls: {0}")]
    Mls(String),
}

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Error::InvalidInput(message.into())
    }

    pub(crate) fn mls(error: impl core::fmt::Debug) -> Self {
        // OpenMLS errors are enums without secrets; Debug is the most precise rendering they offer.
        let mut text = format!("{error:?}");
        text.truncate(300);
        Error::Mls(text)
    }

    /// A stable machine-readable code, shared by every binding.
    pub fn code(&self) -> &'static str {
        match self {
            Error::InvalidInput(_) => "invalid_input",
            Error::Decode(_) => "decode",
            Error::NotAttested => "not_attested",
            Error::InvalidCredential(_) => "invalid_credential",
            Error::UnknownGroup => "unknown_group",
            Error::GroupExists => "group_exists",
            Error::PendingCommit => "pending_commit",
            Error::NoPendingCommit => "no_pending_commit",
            Error::StaleEpoch { .. } => "stale_epoch",
            Error::FutureEpoch { .. } => "future_epoch",
            Error::NotMember => "not_member",
            Error::DuplicateDevice(_) => "duplicate_device",
            Error::MemberNotFound(_) => "member_not_found",
            Error::BindingMismatch(_) => "binding_mismatch",
            Error::DecryptionFailed => "decryption_failed",
            Error::NotSelfGroup => "not_self_group",
            Error::Unsupported(_) => "unsupported",
            Error::Mls(_) => "mls",
        }
    }
}

/// Crate-wide result alias.
pub type Result<T> = core::result::Result<T, Error>;
