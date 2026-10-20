//! # pigeonpost-groupcrypto
//!
//! End-to-end encryption for Pigeonpost persistent groups, built on
//! [OpenMLS](https://github.com/openmls/openmls) (RFC 9420 Messaging Layer Security) with the
//! mandatory-to-implement suite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`.
//!
//! This crate does not invent cryptography. It narrows OpenMLS to the operations Pigeonpost
//! needs and adds the Pigeonpost-specific glue the protocol leaves to applications:
//!
//! * **Identity binding.** Each device credential is attested by the mailbox's ed25519 identity
//!   key, and `/k/` addresses are self-certifying ([`credential`]).
//! * **Message binding.** Every message's authenticated data binds group, sender mailbox and
//!   device, epoch and a client message id ([`MessageAad`]).
//! * **Attachments.** Fresh per-blob keys carried inside the encrypted message ([`attachment`]).
//! * **State at rest.** One blob encrypted under a caller key ([`Device::export_state`]).
//! * **History policy.** New devices get no history; transfer between a mailbox's own devices is
//!   an explicit operation inside a self group ([`Device::seal_history`]).
//!
//! The wire protocol around these objects is specified in `docs/group-e2ee.md`.

#![deny(missing_docs)]

pub mod attachment;
mod codec;
pub mod credential;
mod device;
mod error;
mod payload;
mod random;
mod state;

pub use attachment::{
    open_attachment, seal_attachment, AttachmentRef, SealedAttachment, MAX_ATTACHMENT_BYTES,
};
pub use credential::{
    attestation_payload, device_fingerprint, fingerprint_digits, mailbox_for_seed, safety_number,
    sign_attestation_with_seed, DeviceCredential,
};
pub use device::{
    inspect_envelope, inspect_key_package, welcome_recipients, CommitBundle, CommitOutcome, Device,
    DeviceRef, EnvelopeInfo, HistoryOffer, HistoryTransfer, Incoming, Joined, KeyPackageInfo,
    MemberInfo, OutgoingMessage, ReceivedMessage, CIPHERSUITE, MAX_KEY_PACKAGES_PER_CALL,
};
pub use error::{Error, Result};
pub use payload::{
    validate_client_message_id, verify_envelope_signature, MessageAad, MessageKind,
    MAX_ATTACHMENTS, MAX_BODY_BYTES, MAX_GROUP_ID_BYTES,
};
pub use state::generate_state_key;

/// Version of the Pigeonpost group protocol framing (`enc_scheme` on the wire).
pub const ENC_SCHEME: &str = "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1";
