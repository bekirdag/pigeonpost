//! Purpose-scoped trace epoch keys and their offline-custodian wrapping format.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use curve25519_dalek::montgomery::MontgomeryPoint;
use hkdf::Hkdf;
use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, COMPLIANCE_KEY_ID_LEN};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::{Result, SealError};

/// Exact version of the trace epoch-key wrapping format.
pub const TRACE_KEY_WRAP_VERSION: u8 = 1;
/// HKDF context used only for wrapping trace epoch keys to an offline custodian.
pub const TRACE_KEY_WRAP_HKDF_INFO: &[u8] = b"pigeonpost/trace-key-wrap/v1";
const TRACE_KEY_WRAP_AAD_DOMAIN: &[u8] = b"pigeonpost/trace-key-wrap-aad/v1";
const WRAPPED_KEY_CIPHERTEXT_LEN: usize = 48;
/// Exact length of a canonical wrapped epoch key.
pub const WRAPPED_EPOCH_KEY_LEN: usize = 216;

/// A daily network- or identity-trace sealing key.
///
/// The key is zeroized on drop and cannot represent an attribution key. Attribution uses an
/// independent monthly custody key and never enters the online trace writer.
#[derive(ZeroizeOnDrop)]
pub struct EpochSealingKey {
    #[zeroize(skip)]
    key_id: ComplianceKeyId,
    secret: [u8; 32],
}

impl EpochSealingKey {
    pub fn generate(key_id: ComplianceKeyId) -> Result<Self> {
        let mut secret = [0u8; 32];
        while secret == [0u8; 32] {
            OsRng.fill_bytes(&mut secret);
        }
        Self::from_bytes(key_id, secret)
    }

    pub fn from_bytes(key_id: ComplianceKeyId, secret: [u8; 32]) -> Result<Self> {
        validate_trace_key_id(&key_id)?;
        if secret == [0u8; 32] {
            return Err(SealError::InvalidKey);
        }
        Ok(Self { key_id, secret })
    }

    pub fn key_id(&self) -> ComplianceKeyId {
        self.key_id
    }

    pub(crate) fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    pub(crate) fn commitment(&self) -> [u8; 32] {
        Sha256::digest(self.secret).into()
    }
}

impl core::fmt::Debug for EpochSealingKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EpochSealingKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<withheld>")
            .finish()
    }
}

/// Public, fixed-width envelope carrying one trace epoch key to offline custody.
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedEpochKey {
    version: u8,
    key_id: ComplianceKeyId,
    compliance_key_digest: [u8; 32],
    epoch_key_commitment: [u8; 32],
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 24],
    ciphertext: [u8; WRAPPED_KEY_CIPHERTEXT_LEN],
}

impl WrappedEpochKey {
    /// Wrap an online epoch key to a raw X25519 public key held by the offline custodian.
    pub fn wrap(epoch_key: &EpochSealingKey, compliance_public_key: &[u8; 32]) -> Result<Self> {
        if *compliance_public_key == [0u8; 32] {
            return Err(SealError::InvalidKey);
        }
        let mut ephemeral_secret = [0u8; 32];
        OsRng.fill_bytes(&mut ephemeral_secret);
        let ephemeral_public_key = MontgomeryPoint::mul_base_clamped(ephemeral_secret).to_bytes();
        let shared = Zeroizing::new(
            MontgomeryPoint(*compliance_public_key)
                .mul_clamped(ephemeral_secret)
                .to_bytes(),
        );
        ephemeral_secret.zeroize();
        if *shared == [0u8; 32] {
            return Err(SealError::InvalidKey);
        }

        let compliance_key_digest: [u8; 32] = Sha256::digest(compliance_public_key).into();
        let epoch_key_commitment = epoch_key.commitment();
        let key_id = epoch_key.key_id();
        let aad = trace_key_wrap_aad(
            TRACE_KEY_WRAP_VERSION,
            &key_id,
            &compliance_key_digest,
            &epoch_key_commitment,
            &ephemeral_public_key,
        )?;
        let salt = Zeroizing::new(trace_key_wrap_salt(
            &ephemeral_public_key,
            &compliance_key_digest,
        ));
        let mut aead_key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(Some(&salt[..]), &shared[..])
            .expand(TRACE_KEY_WRAP_HKDF_INFO, &mut *aead_key)
            .map_err(|_| SealError::Crypto)?;
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let encrypted = XChaCha20Poly1305::new((&*aead_key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: epoch_key.secret(),
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::Crypto)?;
        let ciphertext: [u8; WRAPPED_KEY_CIPHERTEXT_LEN] =
            encrypted.try_into().map_err(|_| SealError::Crypto)?;
        Ok(Self {
            version: TRACE_KEY_WRAP_VERSION,
            key_id,
            compliance_key_digest,
            epoch_key_commitment,
            ephemeral_public_key,
            nonce,
            ciphertext,
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != WRAPPED_EPOCH_KEY_LEN || bytes[0] != TRACE_KEY_WRAP_VERSION {
            return Err(SealError::Format);
        }
        let key_id = ComplianceKeyId::decode(&bytes[1..1 + COMPLIANCE_KEY_ID_LEN])
            .map_err(|_| SealError::Format)?;
        validate_trace_key_id(&key_id)?;
        let mut cursor = 1 + COMPLIANCE_KEY_ID_LEN;
        let mut take32 = || {
            let mut value = [0u8; 32];
            value.copy_from_slice(&bytes[cursor..cursor + 32]);
            cursor += 32;
            value
        };
        let compliance_key_digest = take32();
        let epoch_key_commitment = take32();
        let ephemeral_public_key = take32();
        let mut nonce = [0u8; 24];
        nonce.copy_from_slice(&bytes[cursor..cursor + 24]);
        cursor += 24;
        let mut ciphertext = [0u8; WRAPPED_KEY_CIPHERTEXT_LEN];
        ciphertext.copy_from_slice(&bytes[cursor..]);
        if compliance_key_digest == [0u8; 32]
            || epoch_key_commitment == [0u8; 32]
            || ephemeral_public_key == [0u8; 32]
        {
            return Err(SealError::InvalidKey);
        }
        Ok(Self {
            version: TRACE_KEY_WRAP_VERSION,
            key_id,
            compliance_key_digest,
            epoch_key_commitment,
            ephemeral_public_key,
            nonce,
            ciphertext,
        })
    }

    pub fn encode(&self) -> Result<[u8; WRAPPED_EPOCH_KEY_LEN]> {
        if self.version != TRACE_KEY_WRAP_VERSION {
            return Err(SealError::Format);
        }
        validate_trace_key_id(&self.key_id)?;
        let mut out = [0u8; WRAPPED_EPOCH_KEY_LEN];
        out[0] = self.version;
        out[1..48].copy_from_slice(&self.key_id.encode().map_err(|_| SealError::Format)?);
        out[48..80].copy_from_slice(&self.compliance_key_digest);
        out[80..112].copy_from_slice(&self.epoch_key_commitment);
        out[112..144].copy_from_slice(&self.ephemeral_public_key);
        out[144..168].copy_from_slice(&self.nonce);
        out[168..216].copy_from_slice(&self.ciphertext);
        Ok(out)
    }

    pub const fn version(&self) -> u8 {
        self.version
    }

    pub const fn key_id(&self) -> ComplianceKeyId {
        self.key_id
    }

    pub const fn compliance_key_digest(&self) -> [u8; 32] {
        self.compliance_key_digest
    }

    pub const fn epoch_key_commitment(&self) -> [u8; 32] {
        self.epoch_key_commitment
    }

    pub const fn ephemeral_public_key(&self) -> [u8; 32] {
        self.ephemeral_public_key
    }

    pub const fn nonce(&self) -> [u8; 24] {
        self.nonce
    }

    pub const fn ciphertext(&self) -> &[u8; WRAPPED_KEY_CIPHERTEXT_LEN] {
        &self.ciphertext
    }

    /// Rebuild the public AEAD context used by the offline unwrapping implementation.
    pub fn aad(&self) -> Result<Vec<u8>> {
        trace_key_wrap_aad(
            self.version,
            &self.key_id,
            &self.compliance_key_digest,
            &self.epoch_key_commitment,
            &self.ephemeral_public_key,
        )
    }
}

impl core::fmt::Debug for WrappedEpochKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WrappedEpochKey")
            .field("version", &self.version)
            .field("key_id", &self.key_id)
            .field("compliance_key_digest", &self.compliance_key_digest)
            .field("epoch_key_commitment", &self.epoch_key_commitment)
            .field("ephemeral_public_key", &self.ephemeral_public_key)
            .field("nonce", &"<withheld>")
            .field(
                "ciphertext",
                &format_args!("<{} bytes>", self.ciphertext.len()),
            )
            .finish()
    }
}

/// Canonical HKDF salt shared with the offline unwrapping implementation.
pub fn trace_key_wrap_salt(
    ephemeral_public_key: &[u8; 32],
    compliance_key_digest: &[u8; 32],
) -> [u8; 64] {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_public_key);
    salt[32..].copy_from_slice(compliance_key_digest);
    salt
}

fn trace_key_wrap_aad(
    version: u8,
    key_id: &ComplianceKeyId,
    compliance_key_digest: &[u8; 32],
    epoch_key_commitment: &[u8; 32],
    ephemeral_public_key: &[u8; 32],
) -> Result<Vec<u8>> {
    if version != TRACE_KEY_WRAP_VERSION {
        return Err(SealError::Format);
    }
    validate_trace_key_id(key_id)?;
    let key_id = key_id.encode().map_err(|_| SealError::Format)?;
    let mut aad =
        Vec::with_capacity(TRACE_KEY_WRAP_AAD_DOMAIN.len() + 1 + COMPLIANCE_KEY_ID_LEN + 32 * 3);
    aad.extend_from_slice(TRACE_KEY_WRAP_AAD_DOMAIN);
    aad.push(version);
    aad.extend_from_slice(&key_id);
    aad.extend_from_slice(compliance_key_digest);
    aad.extend_from_slice(epoch_key_commitment);
    aad.extend_from_slice(ephemeral_public_key);
    Ok(aad)
}

fn validate_trace_key_id(key_id: &ComplianceKeyId) -> Result<()> {
    key_id.validate().map_err(|_| SealError::Format)?;
    match key_id.purpose {
        CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace => Ok(()),
        CompliancePurpose::Attribution => Err(SealError::WrongPurpose),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigeonpost_compliance_format::Jurisdiction;

    fn id(purpose: CompliancePurpose) -> ComplianceKeyId {
        ComplianceKeyId::new(purpose, Jurisdiction::Test, [7; 32], 1_700_000_000_000, 2)
    }

    #[test]
    fn wrapped_epoch_key_has_one_exact_encoding() {
        let secret = [9u8; 32];
        let public = MontgomeryPoint::mul_base_clamped(secret).to_bytes();
        let epoch =
            EpochSealingKey::from_bytes(id(CompliancePurpose::NetworkTrace), [3; 32]).unwrap();
        let wrapped = WrappedEpochKey::wrap(&epoch, &public).unwrap();
        let encoded = wrapped.encode().unwrap();
        assert_eq!(encoded.len(), WRAPPED_EPOCH_KEY_LEN);
        assert_eq!(WrappedEpochKey::decode(&encoded).unwrap(), wrapped);
        assert!(WrappedEpochKey::decode(&encoded[..215]).is_err());
        let mut extended = encoded.to_vec();
        extended.push(0);
        assert!(WrappedEpochKey::decode(&extended).is_err());
    }

    #[test]
    fn attribution_key_ids_cannot_enter_trace_sealing() {
        assert!(matches!(
            EpochSealingKey::from_bytes(id(CompliancePurpose::Attribution), [1; 32]),
            Err(SealError::WrongPurpose)
        ));
    }

    #[test]
    fn secrets_are_withheld_from_debug() {
        let epoch =
            EpochSealingKey::from_bytes(id(CompliancePurpose::IdentityTrace), [0xA5; 32]).unwrap();
        let debug = format!("{epoch:?}");
        assert!(!debug.contains("165"));
        assert!(debug.contains("withheld"));
    }
}
