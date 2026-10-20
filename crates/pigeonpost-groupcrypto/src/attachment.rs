//! Attachment blobs: each one gets a fresh random 256-bit key and is sealed with
//! XChaCha20-Poly1305. The key, the ciphertext hash and the length travel *inside* the
//! MLS-encrypted message ([`AttachmentRef`]); the server only ever stores the sealed blob.
//!
//! Sealed blob layout: `"PPGA" || u8 version=1 || nonce[24] || ciphertext+tag`.
//! The AEAD associated data is the 5-byte header, so the version cannot be swapped.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::random::random_array;

const MAGIC: &[u8; 4] = b"PPGA";
const VERSION: u8 = 1;
/// Upper bound on one attachment (plaintext), matching the hosted postbox's ceiling.
pub const MAX_ATTACHMENT_BYTES: usize = 100 * 1024 * 1024;

/// Everything a recipient needs to fetch, check and open one attachment.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    /// Fresh XChaCha20-Poly1305 key for this blob only.
    pub key: [u8; 32],
    /// SHA-256 of the sealed blob; the server addresses blobs by this, recipients check it.
    pub sealed_sha256: [u8; 32],
    /// Plaintext length in bytes.
    pub plaintext_len: u64,
}

impl core::fmt::Debug for AttachmentRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AttachmentRef")
            .field("key", &"<redacted>")
            .field("sealed_sha256", &self.sealed_sha256)
            .field("plaintext_len", &self.plaintext_len)
            .finish()
    }
}

impl Drop for AttachmentRef {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// A sealed blob ready for upload, plus the reference to embed in the message.
#[derive(Debug)]
pub struct SealedAttachment {
    /// Bytes to upload.
    pub sealed: Vec<u8>,
    /// Reference to put in [`crate::OutgoingMessage`] attachments.
    pub reference: AttachmentRef,
}

fn header() -> [u8; 5] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

/// Seal one attachment under a fresh random key.
pub fn seal_attachment(plaintext: &[u8]) -> Result<SealedAttachment> {
    if plaintext.len() > MAX_ATTACHMENT_BYTES {
        return Err(Error::invalid("attachment too large"));
    }
    let mut key = random_array::<32>()?;
    let nonce = random_array::<24>()?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let header = header();
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .map_err(|_| Error::invalid("attachment encryption failed"))?;
    let mut sealed = Vec::with_capacity(5 + 24 + ciphertext.len());
    sealed.extend_from_slice(&header);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    let reference = AttachmentRef {
        key,
        sealed_sha256: Sha256::digest(&sealed).into(),
        plaintext_len: plaintext.len() as u64,
    };
    key.zeroize();
    Ok(SealedAttachment { sealed, reference })
}

/// Check the blob's hash against the reference, then open it.
pub fn open_attachment(sealed: &[u8], reference: &AttachmentRef) -> Result<Vec<u8>> {
    let digest: [u8; 32] = Sha256::digest(sealed).into();
    if digest != reference.sealed_sha256 {
        return Err(Error::DecryptionFailed);
    }
    if sealed.len() < 5 + 24 + 16 || sealed[..5] != header() {
        return Err(Error::Decode("not a sealed attachment"));
    }
    let cipher = XChaCha20Poly1305::new((&reference.key).into());
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&sealed[5..29]),
            Payload {
                msg: &sealed[29..],
                aad: &sealed[..5],
            },
        )
        .map_err(|_| Error::DecryptionFailed)?;
    if plaintext.len() as u64 != reference.plaintext_len {
        return Err(Error::DecryptionFailed);
    }
    Ok(plaintext)
}
