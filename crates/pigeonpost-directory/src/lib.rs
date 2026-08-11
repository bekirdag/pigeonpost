//! # Pigeonpost directory
//!
//! The pool of community lofts, and how clients choose among them.
//!
//! This is the component that makes the cost model work. Our own lofts advertise a capacity equal
//! to the budget we chose, so as operators join, our share of new agents falls automatically —
//! no migration, no client update, no decision by anyone (`docs/capacity.md`).
//!
//! Being the directory operator confers as little power as possible: entries are signed by the
//! lofts themselves, probe measurements are published so weights are recomputable, and the
//! directory URL is client configuration rather than a constant.
//!
//! The listener-bound [`serve`] function is the only production HTTP boundary. Raw route
//! construction is deliberately private:
//!
//! ```compile_fail
//! let _ = pigeonpost_directory::server::router;
//! ```

#![forbid(unsafe_code)]

#[cfg(all(test, feature = "server", feature = "test-utilities"))]
extern crate self as pigeonpost_directory;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod directory;
pub mod document;
pub mod entry;
pub mod error;
#[cfg(feature = "server")]
pub mod private_store;
#[cfg(feature = "server")]
pub mod prober;
#[cfg(feature = "server")]
pub mod registry_log;
pub mod selection;
#[cfg(feature = "server")]
mod server;

#[cfg(all(test, feature = "server", feature = "test-utilities"))]
mod pool_tests;

#[cfg(feature = "client")]
pub use client::{canonical_directory_url, verify_snapshot, DirectoryClient, FetchOutcome};
#[cfg(feature = "server")]
pub use directory::{Directory, ProbeResult};
pub use document::DirectoryDocument;
pub use entry::{DirectoryEntry, DrainAuthorization, Health, LoftPolicy, LoftState};
pub use error::{DirectoryError, Result};
#[cfg(feature = "server")]
pub use registry_log::{DirectoryMutationReceipt, MutationInclusionProof, RegistryLogClient};
pub use selection::{rendezvous, select, Rng, SelectionCriteria, TARGET_LOFTS};
#[cfg(all(feature = "server", feature = "test-utilities"))]
#[doc(hidden)]
pub use server::serve_loopback_test;
#[cfg(feature = "server")]
pub use server::{serve, DirectoryHttpConfig, DirectoryLimits, ProbeDocument};
