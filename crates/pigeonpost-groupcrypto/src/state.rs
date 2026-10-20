//! Device state at rest: one blob, encrypted with a caller-provided 256-bit key.
//!
//! ```text
//! "PPGS" || u8 version=1 || nonce[24] || XChaCha20-Poly1305(key, nonce, plaintext, aad = header)
//! ```
//!
//! The plaintext holds the device identity, its MLS signature key, its attestation, the group
//! list, pending-commit markers and every OpenMLS storage entry (ratchet trees, epoch secrets,
//! key package private keys). Callers must overwrite the previous blob after each mutating call:
//! old blobs retain old epoch secrets and so weaken forward secrecy if they leak.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::SignatureScheme;
use openmls_basic_credential::SignatureKeyPair;
use zeroize::Zeroizing;

use crate::codec::{Reader, Writer};
use crate::credential::{canonical_mailbox, validate_device_id, DeviceCredential};
use crate::device::{Device, Provider};
use crate::error::{Error, Result};
use crate::random::random_array;

const MAGIC: &[u8; 4] = b"PPGS";
const VERSION: u8 = 1;
const MAX_STATE_ENTRIES: u32 = 1_000_000;

fn header() -> [u8; 5] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

impl Device {
    /// Serialize and encrypt the whole device state under `key` (32 random bytes kept in the
    /// platform keystore: Keychain, Android Keystore, DPAPI, Secret Service, or a
    /// non-extractable WebCrypto key on the web).
    pub fn export_state(&self, key: &[u8]) -> Result<Vec<u8>> {
        let key: &[u8; 32] = key
            .try_into()
            .map_err(|_| Error::invalid("state key must be 32 bytes"))?;
        let mut w = Writer::new();
        w.bytes16(self.mailbox.as_bytes())?;
        w.bytes16(self.device_id.as_bytes())?;
        let signer = Zeroizing::new(self.signer.tls_serialize_detached().map_err(Error::mls)?);
        w.bytes16(&signer)?;
        match &self.credential {
            Some(c) => {
                w.u8(1);
                w.bytes16(&c.encode()?)?;
            }
            None => {
                w.u8(0);
            }
        }
        w.u32(self.groups.len() as u32);
        for g in &self.groups {
            w.bytes16(g.as_bytes())?;
        }
        w.u32(self.pending_commits.len() as u32);
        for (g, (digest, id)) in &self.pending_commits {
            w.bytes16(g.as_bytes())?;
            w.raw(digest);
            w.bytes16(id.as_bytes())?;
        }
        {
            let values = self
                .provider
                .storage
                .values
                .read()
                .map_err(|_| Error::invalid("state lock poisoned"))?;
            // Sort for deterministic output.
            let sorted: BTreeMap<&Vec<u8>, &Vec<u8>> = values.iter().collect();
            w.u32(sorted.len() as u32);
            for (k, v) in sorted {
                w.bytes32(k)?;
                w.bytes32(v)?;
            }
        }
        let plaintext = Zeroizing::new(w.finish());
        let nonce = random_array::<24>()?;
        let header = header();
        let ciphertext = XChaCha20Poly1305::new(key.into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| Error::invalid("state encryption failed"))?;
        let mut out = Vec::with_capacity(29 + ciphertext.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt and restore a device exported with [`Device::export_state`].
    pub fn import_state(blob: &[u8], key: &[u8]) -> Result<Device> {
        let key: &[u8; 32] = key
            .try_into()
            .map_err(|_| Error::invalid("state key must be 32 bytes"))?;
        if blob.len() < 29 + 16 || blob[..5] != header() {
            return Err(Error::Decode("not a device state blob"));
        }
        let plaintext = Zeroizing::new(
            XChaCha20Poly1305::new(key.into())
                .decrypt(
                    XNonce::from_slice(&blob[5..29]),
                    Payload {
                        msg: &blob[29..],
                        aad: &blob[..5],
                    },
                )
                .map_err(|_| Error::DecryptionFailed)?,
        );
        let mut r = Reader::new(&plaintext);
        let mailbox = canonical_mailbox(r.str16()?)?;
        let device_id = r.str16()?.to_owned();
        validate_device_id(&device_id)?;
        let signer = SignatureKeyPair::tls_deserialize_exact(r.bytes16()?)
            .map_err(|_| Error::Decode("bad signature key"))?;
        if signer.signature_scheme() != SignatureScheme::ED25519 || signer.public().len() != 32 {
            return Err(Error::Decode("unexpected signature scheme"));
        }
        let credential = match r.u8()? {
            0 => None,
            1 => {
                let c = DeviceCredential::decode(r.bytes16()?)?;
                c.verify(signer.public())?;
                if c.mailbox != mailbox || c.device_id != device_id {
                    return Err(Error::Decode("credential does not match device"));
                }
                Some(c)
            }
            _ => return Err(Error::Decode("bad credential flag")),
        };
        let mut groups = BTreeSet::new();
        for _ in 0..r.u32()? {
            groups.insert(r.str16()?.to_owned());
        }
        let mut pending_commits = BTreeMap::new();
        for _ in 0..r.u32()? {
            let g = r.str16()?.to_owned();
            let digest = r.array::<32>()?;
            let id = r.str16()?.to_owned();
            pending_commits.insert(g, (digest, id));
        }
        let count = r.u32()?;
        if count > MAX_STATE_ENTRIES {
            return Err(Error::Decode("too many state entries"));
        }
        let mut values = HashMap::new();
        for _ in 0..count {
            let k = r.bytes32()?.to_vec();
            let v = r.bytes32()?.to_vec();
            values.insert(k, v);
        }
        r.finish()?;
        let provider = Provider::default();
        *provider
            .storage
            .values
            .write()
            .map_err(|_| Error::invalid("state lock poisoned"))? = values;
        Ok(Device {
            provider,
            signer,
            mailbox,
            device_id,
            credential,
            groups,
            pending_commits,
        })
    }
}

/// Generate a fresh random 32-byte key suitable for [`Device::export_state`].
pub fn generate_state_key() -> Result<Vec<u8>> {
    Ok(random_array::<32>()?.to_vec())
}
