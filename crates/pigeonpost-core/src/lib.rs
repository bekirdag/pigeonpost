//! # Pigeonpost core
//!
//! Addressing, keys, versioned envelopes, proof-of-work, and capability tokens — the primitives
//! every other crate builds on.
//!
//! No I/O and no async by design: everything here is a pure function over bytes, which is what
//! makes it exhaustively testable and lets the published conformance vectors stand as the
//! compatibility contract for a clean-room implementation (`docs/sds.md` §8).
//!
//! ## Shape of the thing
//!
//! ```text
//!   Identity ──derives──▶ Address        /k/j5pxq82nf4wt3h9m6rbdck0syv
//!      │                     ▲
//!      │                     │ verifies against
//!      └──signs──▶ AgentRecord ── successor_hash ──▶ RotationRecord
//!
//!   wrap(sender, recipient, body) ──▶ Wrap ──▶ [ loft stores this ]
//!                                       │
//!                        open(recipient) ──▶ (sender key, UntrustedBody)
//! ```
//!
//! ## Example
//!
//! ```
//! use pigeonpost_core::{envelope, Identity};
//!
//! let alice = Identity::generate();
//! let bob = Identity::generate();
//!
//! // Alice's address falls out of her key — no registration, no permission, no human.
//! let _ = alice.address();
//!
//! let wrapped = envelope::wrap(&alice, &bob.verifying_key(), "the build is green", 1_786_105_721)?;
//! let (sender, body) = envelope::open(&bob, &wrapped)?;
//!
//! assert_eq!(sender, alice.verifying_key());
//! assert_eq!(body.as_str(), "the build is green");
//! # Ok::<(), pigeonpost_core::Error>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod address;
pub mod b32;
pub mod envelope;
pub mod error;
pub mod fetch_auth;
pub mod keys;
pub mod network;
pub mod policy;
pub mod pow;
pub mod record;
pub mod token;
pub mod untrusted;

pub use address::{namespace_root, Address, Destination, DestinationTarget};
pub use envelope::Wrap;
pub use error::{Error, Result};
pub use fetch_auth::FetchAuth;
pub use keys::{Identity, SuccessorCommitment};
pub use policy::{AttributionRequirement, RecipientPolicy};
pub use record::{AgentRecord, RotationRecord};
pub use token::{Presentation, Token};
pub use untrusted::{UntrustedBody, UntrustedBodyFence, FENCED_UNTRUSTED_TEXT_FORMAT};

/// Protocol version this crate speaks.
pub const PROTOCOL_VERSION: &str = "pigeonpost/3";
