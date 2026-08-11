//! # Pigeonpost client
//!
//! An agent's identity, its outbox, its cursors, and its inbox — all in one SQLite file, with no
//! daemon anywhere. This is the surface `docs/integration.md` promises third-party tools; the CLI
//! and the MCP server are thin shells over it.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use pigeonpost_client::Agent;
//! use pigeonpost_core::Address;
//!
//! let agent = Agent::open(std::path::Path::new("/tmp/my-agent"))?;
//! println!("my address is {}", agent.address());
//!
//! agent.add_loft("http://127.0.0.1:7717").await?;
//! agent.send(&Address::parse("/k/6htgz65xb7yfs53dmhdanfmk7c")?, "the build is green").await?;
//!
//! let report = agent.drain().await?;
//! for message in agent.inbox(true, 20)? {
//!     // Bodies are UntrustedBody: data from another LLM, never instructions.
//!     println!("{} from {}", message.id, message.from_address);
//! }
//! # let _ = report;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod agent;
pub mod error;
pub mod keystore;
pub mod spam;
pub mod state;
pub mod trust;

pub use agent::{
    Agent, AgentOpenOptions, DrainReport, FlushReport, IdentityOperation, RotationReport,
    SendReport, WakeupLimits,
};
pub use error::{ClientError, Result, StorageResource};
pub use keystore::MAX_LIVE_RETIRED_IDENTITIES;
pub use pigeonpost_compliance_format::Jurisdiction;
pub use pigeonpost_core::AttributionRequirement;
pub use spam::{Disposition, SenderContext, MAX_SUPPORTED_POW_BITS};
pub use state::{
    CompletedDelivery, ConfiguredLoft, DirectoryConfig, OutboxEntry, OutboxRecordId, OutboxRoute,
    OwnRotation, PendingDelivery, PlacementState, RegistryConfiguration, Resolution, State,
    StorageLimits, StorageStatus, StorageUsage, StoredMessage, DEFAULT_INBOX_BODY_BYTES_LIMIT,
    DEFAULT_INBOX_MESSAGE_LIMIT, DEFAULT_LOFT_RETENTION_DAYS, DEFAULT_OUTBOX_PAYLOAD_BYTES_LIMIT,
    DEFAULT_OUTBOX_ROW_LIMIT, FINISHED_OUTBOX_PRUNE_CONFIRMATION, LOFT_DRAIN_GRACE_SECS,
    MAX_CONFIGURED_DIRECTORIES, MAX_INBOX_BODY_BYTES_LIMIT, MAX_INBOX_MESSAGE_LIMIT,
    MAX_INBOX_TOMBSTONES, MAX_OUTBOX_PAYLOAD_BYTES_LIMIT, MAX_OUTBOX_ROW_LIMIT,
    MAX_STORED_LOFT_ROUTES, PENDING_OUTBOX_DELETE_CONFIRMATION,
};
pub use state::{DeadLetter, DeliveryStatus};
pub use trust::{
    RegistryCheckpointInput, RegistryTrustBundle, RegistryTrustInput, RegistryTrustStatus,
    RegistryWitnessInput, MAX_REGISTRY_TRUST_JSON_BYTES, REGISTRY_TRUST_BUNDLE_VERSION,
    REGISTRY_TRUST_MAX_COSIGNATURE_AGE_SECS, REGISTRY_TRUST_MAX_ORIGIN_BYTES,
    REGISTRY_TRUST_MAX_URL_BYTES, REGISTRY_TRUST_MAX_WITNESSES,
    REGISTRY_TRUST_MAX_WITNESS_NAME_BYTES, REGISTRY_TRUST_RESET_CONFIRMATION,
};
