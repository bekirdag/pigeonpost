use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LoftError {
    #[error(transparent)]
    Core(#[from] pigeonpost_core::Error),

    #[error("storage: {0}")]
    Storage(#[from] rusqlite::Error),

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("loft is at capacity")]
    AtCapacity,

    #[error("rate limited")]
    RateLimited,

    #[error("loft is busy")]
    Overloaded,

    #[error("policy changed during admission")]
    PolicyChanged,

    #[error("attribution rejected")]
    AttributionRejected,

    #[error("attribution verification is temporarily unavailable")]
    AttributionUnavailable,

    #[error("trace capture unavailable")]
    TraceUnavailable,

    #[error("invalid trace segment metadata")]
    TraceMetadata,

    #[error("loft is not ready")]
    NotReady,

    #[error("unsupported database schema version {found}; newest supported is {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },

    #[error("invalid loft configuration: {0}")]
    Configuration(&'static str),

    #[error("not found")]
    NotFound,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, LoftError>;

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for LoftError {
    fn into_response(self) -> Response {
        use pigeonpost_core::Error as CoreError;

        let status = match &self {
            // A rejected proof is indistinguishable from a wrong key, deliberately: a prober or
            // an attacker learns only "no", never which check failed.
            LoftError::Core(CoreError::BadSignature)
            | LoftError::Core(CoreError::StaleTimestamp)
            | LoftError::Core(CoreError::InvalidKey) => StatusCode::UNAUTHORIZED,

            // Proof-of-work is a recipient policy, never a fee or payment path.
            LoftError::Core(CoreError::InsufficientWork) => StatusCode::FORBIDDEN,
            LoftError::Core(CoreError::StaleSequence) => StatusCode::CONFLICT,
            LoftError::Core(CoreError::TooLarge) => StatusCode::PAYLOAD_TOO_LARGE,
            LoftError::Core(_) => StatusCode::BAD_REQUEST,

            LoftError::AtCapacity => StatusCode::INSUFFICIENT_STORAGE,
            LoftError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            LoftError::Overloaded => StatusCode::TOO_MANY_REQUESTS,
            LoftError::PolicyChanged => StatusCode::CONFLICT,
            LoftError::AttributionRejected => StatusCode::BAD_REQUEST,
            LoftError::AttributionUnavailable
            | LoftError::TraceUnavailable
            | LoftError::NotReady => StatusCode::SERVICE_UNAVAILABLE,
            LoftError::NotFound => StatusCode::NOT_FOUND,

            LoftError::Storage(_)
            | LoftError::Serialization(_)
            | LoftError::Io(_)
            | LoftError::TraceMetadata
            | LoftError::UnsupportedSchema { .. }
            | LoftError::Configuration(_) => {
                // Do not put paths, network identifiers, SQL, or attacker-controlled values in
                // ordinary logs. The coarse kind is enough for alert routing.
                tracing::error!(kind = self.kind(), "loft internal error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        let message = match status {
            StatusCode::INTERNAL_SERVER_ERROR => "internal error".to_string(),
            // Uniform text as well as a uniform status: the body must not reveal whether the
            // signature, the clock, or the loft binding was what failed.
            StatusCode::UNAUTHORIZED => "unauthorized".to_string(),
            StatusCode::BAD_REQUEST => "invalid request".to_string(),
            StatusCode::CONFLICT => "conflict".to_string(),
            StatusCode::SERVICE_UNAVAILABLE => "temporarily unavailable".to_string(),
            _ => self.to_string(),
        };

        (status, axum::Json(ErrorBody { error: message })).into_response()
    }
}

impl LoftError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Core(_) => "core",
            Self::Storage(_) => "storage",
            Self::Serialization(_) => "serialization",
            Self::AtCapacity => "capacity",
            Self::RateLimited => "rate_limit",
            Self::Overloaded => "overload",
            Self::PolicyChanged => "policy_changed",
            Self::AttributionRejected => "attribution",
            Self::AttributionUnavailable => "attribution_readiness",
            Self::TraceUnavailable => "trace",
            Self::TraceMetadata => "trace_metadata",
            Self::NotReady => "readiness",
            Self::UnsupportedSchema { .. } => "schema",
            Self::Configuration(_) => "configuration",
            Self::NotFound => "not_found",
            Self::Io(_) => "io",
        }
    }
}
