//! # Pigeonpost registry
//!
//! Human-readable handles — `/github/superaidev` — bound to keys in an append-only transparency log.
//!
//! This is the *only* tier that needs a registry at all. Key addresses are self-certifying and
//! never touch this component, which keeps the v0.2 supported log envelope small and free of
//! unauthenticated allocation pressure. Larger deployments require the authenticated snapshot/map
//! protocol described in the design documents; they are not hidden behind unbounded client work.
//!
//! ## What makes it not ours
//!
//! - Every entry is appended, never edited, so the binding history is publicly auditable
//! - Every resolve returns an **inclusion proof** the caller verifies itself
//! - A **consistency proof** lets a witness prove the log only ever appended — which is how a
//!   rewrite is caught rather than trusted-not-to-happen
//! - The whole log downloads as a file, so a fork keeps every name
//!
//! The normal API never returns an Axum router. An unwitnessed development service is explicit,
//! read-only, and verifies the already-bound listener is loopback:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use pigeonpost_registry::*;
//! # async fn example() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! let signing_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
//! let registry = Arc::new(Registry::in_memory(RegistryConfig {
//!     origin: "pigeonpost.dev/registry".into(),
//!     signing_key,
//!     allow_mock_identities: false,
//! })?);
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
//! let (stop, stopped) = tokio::sync::watch::channel(false);
//! let task = tokio::spawn(serve_loopback_read_only(
//!     listener,
//!     registry,
//!     RegistryHttpConfig::direct(),
//!     stopped,
//! ));
//! stop.send(true)?;
//! task.await??;
//! # Ok(())
//! # }
//! ```
//!
//! Raw route construction is deliberately private, including with test utilities enabled:
//!
//! ```compile_fail
//! let _ = pigeonpost_registry::server::router;
//! ```

#![forbid(unsafe_code)]

/// Pinned public authorization endpoint used by clients before beginning GitHub OAuth2.
pub const GITHUB_AUTHORIZATION_ENDPOINT: &str = "https://github.com/login/oauth/authorize";
/// Pinned public authorization endpoint used by clients before beginning Google OIDC.
pub const GOOGLE_AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Exact number of immutable log leaves carried by one client bootstrap stream request.
///
/// The value is part of the v0.2 HTTP capacity contract: it keeps a one-million-leaf bootstrap to
/// 123 cacheable requests while retaining bounded retry and transfer state per request.
pub const AUDIT_DUMP_SEGMENT_ENTRIES: u64 = 8_192;

pub mod checkpoint;
#[cfg(feature = "server")]
pub mod claim_trace;
#[cfg(feature = "client")]
pub mod client;
pub mod directory_publisher;
pub mod entry;
pub mod error;
pub mod handle;
#[cfg(feature = "server")]
pub mod identity;
pub mod log;
#[cfg(feature = "server")]
pub mod registry;
pub mod reserved;
#[cfg(feature = "server")]
mod server;
#[cfg(feature = "server")]
mod storage;
#[cfg(feature = "server")]
pub mod witness;

pub use checkpoint::{witness_quorum_intersects, Checkpoint, VerifiedCheckpoint, WitnessKey};
#[cfg(feature = "client")]
pub use client::{
    validate_handle_transition, AuditedComplianceKey, AuditedHandleBinding, AuditedHandleMutation,
    CheckpointPin, ComplianceAuditState, HandleAuditState, HandleProjectionStore,
    HandlePublication, RegistryClient, RegistryTrust, VerifiedComplianceKey,
    VerifiedComplianceKeys, VerifiedHandle, REGISTRY_AUDIT_TOTAL_TIMEOUT,
};
pub use entry::{
    ComplianceKeyPublish, ComplianceKeyStatus, DirectoryAdd, DirectoryRemove, EntryKind,
    HandleClaim, HandleRotation, LogEntry, Versioned,
};
pub use error::{RegistryError, Result};
pub use handle::Handle;
pub use log::{verify_consistency, verify_inclusion, MerkleFrontier, MerkleLog};
#[cfg(feature = "server")]
pub use registry::{
    AuthorizationMetadata, ComplianceKeyQuery, ComplianceKeySet, IdentityChallenge,
    IdentityChallengeProvider, LoggedComplianceKey, LoggedDirectoryMutation, ProofBundle,
    Registration, RegistrationLimits, Registry, RegistryConfig, Resolved, WitnessPublicationStatus,
    MAX_REGISTRATION_BINDINGS_PER_MINUTE,
};
#[cfg(feature = "server")]
pub use server::{
    serve_loopback_read_only, serve_witnessed, RegistryHttpConfig, RegistryLimits,
    MAX_REGISTRY_BLOCKING_OPERATIONS, MAX_REGISTRY_BLOCKING_TIMEOUT_MS,
    MAX_REGISTRY_CONCURRENT_CONNECTIONS, MAX_REGISTRY_CONCURRENT_REQUESTS,
    MAX_REGISTRY_DUMP_STREAMS, MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE,
    MAX_REGISTRY_HEADER_TIMEOUT_MS, MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE,
    MAX_REGISTRY_SOURCE_KEYS, MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE,
};
#[cfg(all(feature = "server", feature = "test-utilities"))]
#[doc(hidden)]
pub use server::{serve_loopback_test, serve_loopback_test_supervised};
#[cfg(feature = "server")]
pub use witness::{
    WitnessClient, WitnessConfig, WitnessError, WitnessPolicy, WitnessReceipt, WitnessResult,
    WitnessSupervisor, WitnessSyncReport, WitnessTiming,
};
