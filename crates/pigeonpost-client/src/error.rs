use thiserror::Error;

/// The independently bounded client-state resource which rejected a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageResource {
    InboxMessages,
    InboxTombstones,
    InboxBodyBytes,
    OutboxRows,
    OutboxPayloadBytes,
}

impl core::fmt::Display for StorageResource {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::InboxMessages => "inbox message count",
            Self::InboxTombstones => "inbox replay tombstones",
            Self::InboxBodyBytes => "inbox body bytes",
            Self::OutboxRows => "outbox row count",
            Self::OutboxPayloadBytes => "outbox payload bytes",
        })
    }
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Core(#[from] pigeonpost_core::Error),

    #[error(transparent)]
    Loft(#[from] pigeonpost_loft::ClientError),

    #[error(transparent)]
    Directory(#[from] pigeonpost_directory::DirectoryError),

    #[error(transparent)]
    Registry(#[from] pigeonpost_registry::RegistryError),

    #[error("state: {0}")]
    State(#[from] rusqlite::Error),

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("no identity yet — run `pigeonpost id` to create one")]
    NoIdentity,

    #[error("no lofts configured — add one with `pigeonpost loft add <url>`")]
    NoLofts,

    #[error("could not find {0}: no loft we asked is holding a record for that address")]
    Unresolvable(String),

    #[error("message {0} not found")]
    NoSuchMessage(String),

    #[error("message prefix {0} is ambiguous; provide more of the id")]
    AmbiguousMessage(String),

    #[error("could not deliver to any of the recipient's lofts")]
    Undeliverable,

    #[error("client storage limit reached for {0}")]
    StorageLimit(StorageResource),

    #[error("recipient policy reached only {succeeded} of {total} configured lofts")]
    PolicyIncomplete { succeeded: usize, total: usize },

    #[error(
        "required attribution key is not present in the witnessed prefix; retry after registry refresh"
    )]
    AttributionTrustUnavailable,

    #[error("{0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;
