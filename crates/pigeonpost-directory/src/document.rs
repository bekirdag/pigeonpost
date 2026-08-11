//! Signed, client-verifiable directory snapshot shared without server dependencies.

use std::collections::HashSet;

use ed25519_dalek::Signature;
use pigeonpost_core::keys;
use serde::{Deserialize, Serialize};

#[cfg(feature = "server")]
use crate::directory::Directory;
#[cfg(feature = "server")]
use crate::entry::hex;
use crate::entry::{parse_hex32, parse_hex64, DirectoryEntry, LoftState};
use crate::error::{DirectoryError, Result};

const DIRECTORY_DOCUMENT_DOMAIN: &[u8] = b"pigeonpost/directory-document/v1";
pub const MAX_DIRECTORY_ENTRIES: usize = 512;
pub(crate) const MAX_DIRECTORY_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryDocument {
    pub version: u32,
    pub generated_at: u64,
    pub lofts: Vec<DirectoryEntry>,
    /// Pin this key in client configuration; carrying it here does not make it trusted.
    pub signing_key: String,
    pub signature: String,
}

impl DirectoryDocument {
    #[cfg(feature = "server")]
    pub(crate) fn signed(
        directory: &Directory,
        generated_at: u64,
        lofts: Vec<DirectoryEntry>,
    ) -> Result<Self> {
        if lofts.len() > MAX_DIRECTORY_ENTRIES {
            return Err(DirectoryError::ResponseTooLarge);
        }
        let signing_key = hex(&directory.signing_public_key());
        let payload = directory_document_payload(1, generated_at, &lofts, &signing_key)?;
        let document = Self {
            version: 1,
            generated_at,
            lofts,
            signing_key,
            signature: hex(&directory.sign(&payload)),
        };
        if serde_json::to_vec(&document)?.len() > MAX_DIRECTORY_DOCUMENT_BYTES {
            return Err(DirectoryError::ResponseTooLarge);
        }
        Ok(document)
    }

    pub fn verify(&self, expected_key: &[u8; 32]) -> Result<()> {
        if self.version != 1 {
            return Err(DirectoryError::Malformed(
                "unsupported directory document version".into(),
            ));
        }
        if self.lofts.len() > MAX_DIRECTORY_ENTRIES {
            return Err(DirectoryError::Malformed(
                "directory document contains too many lofts".into(),
            ));
        }
        if serde_json::to_vec(self)?.len() > MAX_DIRECTORY_DOCUMENT_BYTES {
            return Err(DirectoryError::ResponseTooLarge);
        }
        verify_document_signature(
            expected_key,
            &self.signing_key,
            &self.signature,
            &directory_document_payload(
                self.version,
                self.generated_at,
                &self.lofts,
                &self.signing_key,
            )?,
        )?;

        let mut endpoints = HashSet::with_capacity(self.lofts.len());
        for loft in &self.lofts {
            loft.verify()?;
            if matches!(loft.state, LoftState::Pending | LoftState::Removed) {
                return Err(DirectoryError::Malformed(
                    "directory document contains a non-public loft state".into(),
                ));
            }
            if !endpoints.insert(&loft.endpoint) {
                return Err(DirectoryError::Malformed(
                    "directory document contains a duplicate endpoint".into(),
                ));
            }
            if loft.last_mutation_sequence < loft.sequence {
                return Err(DirectoryError::Malformed(
                    "directory observation predates the signed loft mutation".into(),
                ));
            }
        }
        Ok(())
    }
}

fn directory_document_payload(
    version: u32,
    generated_at: u64,
    lofts: &[DirectoryEntry],
    signing_key: &str,
) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(DIRECTORY_DOCUMENT_DOMAIN.len() + 128);
    payload.extend_from_slice(DIRECTORY_DOCUMENT_DOMAIN);
    payload.extend_from_slice(&version.to_le_bytes());
    payload.extend_from_slice(&generated_at.to_le_bytes());
    push_field(&mut payload, signing_key.as_bytes());
    push_field(&mut payload, &serde_json::to_vec(lofts)?);
    Ok(payload)
}

pub(crate) fn verify_document_signature(
    expected_key: &[u8; 32],
    advertised_key: &str,
    signature: &str,
    payload: &[u8],
) -> Result<()> {
    let advertised = parse_hex32(advertised_key)
        .ok_or_else(|| DirectoryError::Malformed("directory key must be 32 hex bytes".into()))?;
    if &advertised != expected_key {
        return Err(DirectoryError::KeyMismatch);
    }
    let key = keys::verifying_key_from_bytes(expected_key)
        .map_err(|_| DirectoryError::Malformed("directory key is invalid".into()))?;
    let signature = parse_hex64(signature)
        .ok_or_else(|| DirectoryError::Malformed("signature must be 64 hex bytes".into()))?;
    keys::verify(&key, payload, &Signature::from_bytes(&signature))
        .map_err(|_| DirectoryError::BadSignature)
}

pub(crate) fn push_field(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u64).to_le_bytes());
    out.extend_from_slice(field);
}
