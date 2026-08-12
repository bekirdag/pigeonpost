#[cfg(feature = "server")]
use axum::http::StatusCode;
#[cfg(feature = "server")]
use axum::response::{IntoResponse, Response};
#[cfg(feature = "server")]
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("unknown namespace: {0}")]
    UnknownNamespace(String),

    #[error("malformed handle: {0}")]
    MalformedHandle(String),

    #[error("malformed checkpoint: {0}")]
    MalformedCheckpoint(String),

    #[error("malformed log entry: {0}")]
    MalformedEntry(String),

    #[error("registry schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },

    #[error("registry migration requires operator authorization: {0}")]
    MigrationRequired(String),

    #[error("registry storage integrity check failed: {0}")]
    CorruptStorage(String),

    #[error("this proof is for a different provider")]
    WrongProvider,

    #[error("identity provider is not configured")]
    ProviderNotConfigured,

    #[error("invalid registry configuration: {0}")]
    InvalidConfiguration(String),

    #[error("identity provider unreachable: {0}")]
    ProviderUnreachable(String),

    #[error("registry unavailable")]
    RegistryUnavailable,

    #[error("registry witness quorum unavailable")]
    WitnessUnavailable,

    #[error("registry witness state rolled back or equivocated")]
    WitnessConflict,

    #[error("claim trace unavailable")]
    ClaimTraceUnavailable,

    #[error("request rate limit exceeded")]
    RateLimited,

    #[error("registry is overloaded")]
    Overloaded,

    #[error("identity proof rejected: {0}")]
    ProofRejected(String),

    /// The proof authenticated someone, but not the person whose handle is being claimed.
    #[error("proof is for '{proved}', which cannot claim '{claimed}'")]
    SubjectMismatch { proved: String, claimed: String },

    #[error("the request is not signed by the key being bound")]
    KeyPossessionNotProved,

    #[error("directory publisher authorization failed")]
    DirectoryPublisherUnauthorized,

    #[error("this account already holds the maximum of {limit} handles")]
    HandleQuotaExceeded { limit: usize },

    #[error("flat handle registration is not yet available")]
    HandleTierUnavailable,

    #[error("handle is already bound to a different key")]
    AlreadyBound,

    #[error("directory endpoint is bound to a different loft key")]
    DirectoryKeyMismatch,

    #[error("directory mutation sequence was replayed or equivocated")]
    DirectoryReplay,

    #[error("handle not found")]
    NotFound,

    #[error(transparent)]
    Core(#[from] pigeonpost_core::Error),

    #[error("storage: {0}")]
    #[cfg(feature = "server")]
    Storage(#[from] rusqlite::Error),

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, RegistryError>;

#[cfg(feature = "server")]
#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

#[cfg(feature = "server")]
impl IntoResponse for RegistryError {
    fn into_response(self) -> Response {
        let status = match &self {
            RegistryError::UnknownNamespace(_)
            | RegistryError::MalformedHandle(_)
            | RegistryError::MalformedCheckpoint(_)
            | RegistryError::MalformedEntry(_)
            | RegistryError::WrongProvider
            | RegistryError::Core(_) => StatusCode::BAD_REQUEST,

            RegistryError::ProofRejected(_)
            | RegistryError::SubjectMismatch { .. }
            | RegistryError::KeyPossessionNotProved
            | RegistryError::DirectoryPublisherUnauthorized
            | RegistryError::DirectoryKeyMismatch => StatusCode::UNAUTHORIZED,

            RegistryError::AlreadyBound
            | RegistryError::HandleQuotaExceeded { .. }
            | RegistryError::DirectoryReplay
            | RegistryError::WitnessConflict => StatusCode::CONFLICT,
            RegistryError::NotFound => StatusCode::NOT_FOUND,
            RegistryError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            RegistryError::Overloaded | RegistryError::HandleTierUnavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            RegistryError::ProviderNotConfigured => StatusCode::NOT_IMPLEMENTED,
            RegistryError::ProviderUnreachable(_) | RegistryError::RegistryUnavailable => {
                StatusCode::BAD_GATEWAY
            }
            RegistryError::WitnessUnavailable | RegistryError::ClaimTraceUnavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }

            RegistryError::UnsupportedSchema { .. }
            | RegistryError::MigrationRequired(_)
            | RegistryError::CorruptStorage(_)
            | RegistryError::InvalidConfiguration(_)
            | RegistryError::Storage(_)
            | RegistryError::Serialization(_)
            | RegistryError::Io(_) => {
                tracing::error!(kind = self.kind(), "registry internal error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            "internal error".to_string()
        } else {
            self.to_string()
        };

        (status, axum::Json(ErrorBody { error: message })).into_response()
    }
}

#[cfg(feature = "server")]
impl RegistryError {
    fn kind(&self) -> &'static str {
        match self {
            Self::UnknownNamespace(_) => "namespace",
            Self::MalformedHandle(_) => "handle",
            Self::MalformedCheckpoint(_) => "checkpoint",
            Self::MalformedEntry(_) => "entry",
            Self::UnsupportedSchema { .. } => "schema",
            Self::MigrationRequired(_) => "migration",
            Self::CorruptStorage(_) => "storage_integrity",
            Self::WrongProvider => "provider",
            Self::ProviderNotConfigured => "provider_config",
            Self::InvalidConfiguration(_) => "configuration",
            Self::ProviderUnreachable(_) => "provider_transport",
            Self::RegistryUnavailable => "registry_transport",
            Self::WitnessUnavailable => "witness_unavailable",
            Self::WitnessConflict => "witness_conflict",
            Self::ClaimTraceUnavailable => "claim_trace",
            Self::RateLimited => "rate_limit",
            Self::Overloaded => "overloaded",
            Self::ProofRejected(_) => "proof",
            Self::SubjectMismatch { .. } => "subject",
            Self::KeyPossessionNotProved => "key_possession",
            Self::DirectoryPublisherUnauthorized => "directory_publisher",
            Self::AlreadyBound => "already_bound",
            Self::HandleQuotaExceeded { .. } => "handle_quota",
            Self::HandleTierUnavailable => "handle_tier_unavailable",
            Self::DirectoryKeyMismatch => "directory_key_mismatch",
            Self::DirectoryReplay => "directory_replay",
            Self::NotFound => "not_found",
            Self::Core(_) => "core",
            Self::Storage(_) => "storage",
            Self::Serialization(_) => "serialization",
            Self::Io(_) => "io",
        }
    }
}
