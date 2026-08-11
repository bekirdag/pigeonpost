#[cfg(feature = "server")]
use axum::http::StatusCode;
#[cfg(feature = "server")]
use axum::response::{IntoResponse, Response};
#[cfg(feature = "server")]
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DirectoryError {
    #[error("malformed entry: {0}")]
    Malformed(String),

    #[error("entry is not signed by the key it names")]
    BadSignature,

    #[error("key does not match the configured or previously bound key")]
    KeyMismatch,

    #[error("directory signing key is not provisioned")]
    SigningKeyNotProvisioned,

    #[error("mutation sequence must be strictly greater than the stored sequence")]
    Replay,

    #[error("no such loft")]
    NotFound,

    #[error("directory service is unavailable")]
    Unavailable,

    #[error("directory service is overloaded")]
    Overloaded,

    #[error("directory request rate limit exceeded")]
    RateLimited,

    #[error("directory service is not ready")]
    NotReady,

    #[error("registry witness publication timed out")]
    RegistryPublicationTimeout,

    #[error("registry proof rejected: {0}")]
    RegistryProof(String),

    #[error("directory document is stale or from the future")]
    StaleDocument,

    #[error("directory response exceeds the configured limit")]
    ResponseTooLarge,

    #[cfg(feature = "server")]
    #[error("storage: {0}")]
    Storage(#[from] rusqlite::Error),

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DirectoryError>;

#[cfg(feature = "server")]
#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

#[cfg(feature = "server")]
impl IntoResponse for DirectoryError {
    fn into_response(self) -> Response {
        let status = match &self {
            DirectoryError::Malformed(_) => StatusCode::BAD_REQUEST,
            DirectoryError::BadSignature | DirectoryError::KeyMismatch => StatusCode::UNAUTHORIZED,
            DirectoryError::Replay => StatusCode::CONFLICT,
            DirectoryError::NotFound => StatusCode::NOT_FOUND,
            DirectoryError::Unavailable
            | DirectoryError::Overloaded
            | DirectoryError::NotReady
            | DirectoryError::RegistryPublicationTimeout => StatusCode::SERVICE_UNAVAILABLE,
            DirectoryError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            DirectoryError::RegistryProof(_) => StatusCode::BAD_GATEWAY,
            DirectoryError::StaleDocument => StatusCode::BAD_GATEWAY,
            DirectoryError::ResponseTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            DirectoryError::Storage(_)
            | DirectoryError::Serialization(_)
            | DirectoryError::SigningKeyNotProvisioned
            | DirectoryError::Io(_) => {
                tracing::error!(kind = self.kind(), "directory internal error");
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
impl DirectoryError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::BadSignature => "signature",
            Self::KeyMismatch => "key_mismatch",
            Self::SigningKeyNotProvisioned => "signing_key_not_provisioned",
            Self::Replay => "replay",
            Self::NotFound => "not_found",
            Self::Unavailable => "unavailable",
            Self::Overloaded => "overloaded",
            Self::RateLimited => "rate_limited",
            Self::NotReady => "not_ready",
            Self::RegistryPublicationTimeout => "registry_publication_timeout",
            Self::RegistryProof(_) => "registry_proof",
            Self::StaleDocument => "stale_document",
            Self::ResponseTooLarge => "response_size",
            Self::Storage(_) => "storage",
            Self::Serialization(_) => "serialization",
            Self::Io(_) => "io",
        }
    }
}
