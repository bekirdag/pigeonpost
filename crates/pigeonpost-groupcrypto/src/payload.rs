//! Pigeonpost-specific framing around MLS: the authenticated data (AAD) bound into every
//! handshake and application message, the encrypted application payload, and the detached
//! envelope signature the server can check without decrypting anything.

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::attachment::AttachmentRef;
use crate::codec::{Reader, Writer};
use crate::error::{Error, Result};

const AAD_MAGIC: &[u8; 4] = b"PPGC";
const AAD_VERSION: u8 = 1;
const PAYLOAD_VERSION: u8 = 1;
const ENVELOPE_DOMAIN: &[u8] = b"pigeonpost/groupcrypto/envelope/v1\0";
/// Largest application body (bytes) a message may carry.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Most attachments per message.
pub const MAX_ATTACHMENTS: usize = 32;
const MAX_CLIENT_MESSAGE_ID: usize = 64;
/// Largest MLS group id accepted (Pigeonpost uses `grp_<26>`).
pub const MAX_GROUP_ID_BYTES: usize = 64;

/// What a protected MLS message carries, as declared in its (cleartext, authenticated) AAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageKind {
    /// An encrypted application message.
    Application = 1,
    /// An encrypted commit (membership change or key update).
    Commit = 2,
}

/// The authenticated data of every message this crate produces. It is sent in clear inside
/// the MLS `PrivateMessage` and integrity-protected by the AEAD, so the server can route and
/// validate it and recipients can trust it after decryption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageAad {
    /// Application or commit.
    pub kind: MessageKind,
    /// MLS group id (the Pigeonpost `grp_…` id bytes).
    pub group_id: Vec<u8>,
    /// Sender mailbox (`/k/…`).
    pub sender_mailbox: String,
    /// Sender device id.
    pub sender_device: String,
    /// Epoch in which the message was created.
    pub epoch: u64,
    /// Client-chosen unique id, used for idempotency and replay detection.
    pub client_message_id: String,
}

/// Validate a client message id: 1..=64 characters of `[A-Za-z0-9_-]`.
pub fn validate_client_message_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= MAX_CLIENT_MESSAGE_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(
            "client message id must be 1-64 characters of A-Z a-z 0-9 _ -",
        ))
    }
}

pub(crate) fn validate_group_id(group_id: &[u8]) -> Result<()> {
    if group_id.is_empty() || group_id.len() > MAX_GROUP_ID_BYTES {
        return Err(Error::invalid("group id must be 1-64 bytes"));
    }
    Ok(())
}

impl MessageAad {
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer::new();
        w.raw(AAD_MAGIC).u8(AAD_VERSION).u8(self.kind as u8);
        w.bytes16(&self.group_id)?;
        w.bytes16(self.sender_mailbox.as_bytes())?;
        w.bytes16(self.sender_device.as_bytes())?;
        w.u64(self.epoch);
        w.bytes16(self.client_message_id.as_bytes())?;
        Ok(w.finish())
    }

    /// Parse AAD bytes taken from a protected message.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.take(4)? != AAD_MAGIC || r.u8()? != AAD_VERSION {
            return Err(Error::Decode("unknown authenticated-data format"));
        }
        let kind = match r.u8()? {
            1 => MessageKind::Application,
            2 => MessageKind::Commit,
            _ => return Err(Error::Decode("unknown message kind")),
        };
        let group_id = r.bytes16()?.to_vec();
        let sender_mailbox = r.str16()?.to_owned();
        let sender_device = r.str16()?.to_owned();
        let epoch = r.u64()?;
        let client_message_id = r.str16()?.to_owned();
        r.finish()?;
        validate_client_message_id(&client_message_id)?;
        Ok(Self {
            kind,
            group_id,
            sender_mailbox,
            sender_device,
            epoch,
            client_message_id,
        })
    }
}

/// Application payload kinds inside the encryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum PayloadKind {
    Message = 1,
    HistoryOffer = 2,
}

pub(crate) struct Payload {
    pub kind: PayloadKind,
    pub client_message_id: String,
    pub body: Vec<u8>,
    pub attachments: Vec<AttachmentRef>,
}

impl Payload {
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        if self.body.len() > MAX_BODY_BYTES {
            return Err(Error::invalid("message body too large"));
        }
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(Error::invalid("too many attachments"));
        }
        let mut w = Writer::new();
        w.u8(PAYLOAD_VERSION).u8(self.kind as u8);
        w.bytes16(self.client_message_id.as_bytes())?;
        w.bytes32(&self.body)?;
        w.u16(self.attachments.len() as u16);
        for a in &self.attachments {
            w.raw(&a.key).raw(&a.sealed_sha256).u64(a.plaintext_len);
        }
        Ok(w.finish())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.u8()? != PAYLOAD_VERSION {
            return Err(Error::Decode("unknown payload version"));
        }
        let kind = match r.u8()? {
            1 => PayloadKind::Message,
            2 => PayloadKind::HistoryOffer,
            _ => return Err(Error::Decode("unknown payload kind")),
        };
        let client_message_id = r.str16()?.to_owned();
        let body = r.bytes32()?.to_vec();
        if body.len() > MAX_BODY_BYTES {
            return Err(Error::Decode("message body too large"));
        }
        let count = usize::from(r.u16()?);
        if count > MAX_ATTACHMENTS {
            return Err(Error::Decode("too many attachments"));
        }
        let mut attachments = Vec::with_capacity(count);
        for _ in 0..count {
            attachments.push(AttachmentRef {
                key: r.array()?,
                sealed_sha256: r.array()?,
                plaintext_len: r.u64()?,
            });
        }
        r.finish()?;
        Ok(Self {
            kind,
            client_message_id,
            body,
            attachments,
        })
    }
}

pub(crate) fn envelope_signing_input(message: &[u8]) -> Vec<u8> {
    let mut input = ENVELOPE_DOMAIN.to_vec();
    input.extend_from_slice(&Sha256::digest(message));
    input
}

/// Verify a detached envelope signature (see [`crate::OutgoingMessage::envelope_signature`])
/// against a device's MLS signature public key. This is what the postbox runs on upload: it
/// proves which enrolled device produced the ciphertext without reading it.
pub fn verify_envelope_signature(
    device_signature_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<()> {
    let key: [u8; 32] = device_signature_key
        .try_into()
        .map_err(|_| Error::invalid("device signature key must be 32 bytes"))?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| Error::invalid("signature must be 64 bytes"))?;
    let key = VerifyingKey::from_bytes(&key)
        .map_err(|_| Error::invalid("invalid device signature key"))?;
    key.verify_strict(
        &envelope_signing_input(message),
        &Signature::from_bytes(&signature),
    )
    .map_err(|_| Error::BindingMismatch("envelope signature does not verify"))
}
