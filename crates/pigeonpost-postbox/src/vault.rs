//! Key vault — wrap an identity's secret seed at rest (plan §8, managed custody).
//!
//! P0 seals the 32-byte Ed25519 seed under a master key with XChaCha20-Poly1305. The master key is
//! derived from a sealed file on disk (`POSTBOX_KMS=sealed-file:/path`); production replaces that
//! with an age envelope or a KMS/HSM. The plaintext seed is only ever unwrapped into short-lived
//! memory to sign or open envelopes, then zeroized by the caller.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    Key, XChaCha20Poly1305, XNonce,
};

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("failed to seal key")]
    Seal,
    #[error("failed to open key")]
    Open,
}

/// A sealed secret: the AEAD nonce and ciphertext. Storable as-is.
#[derive(Clone)]
pub struct Wrapped {
    pub nonce: [u8; 24],
    pub ct: Vec<u8>,
}

/// Seals and opens identity seeds under a single master key.
pub struct Vault {
    cipher: XChaCha20Poly1305,
}

impl Vault {
    pub fn new(master: [u8; 32]) -> Self {
        Vault {
            cipher: XChaCha20Poly1305::new(Key::from_slice(&master)),
        }
    }

    /// Seal a 32-byte seed.
    pub fn wrap(&self, seed: &[u8; 32]) -> Result<Wrapped, VaultError> {
        let mut nonce = [0u8; 24];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
        let ct = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), seed.as_slice())
            .map_err(|_| VaultError::Seal)?;
        Ok(Wrapped { nonce, ct })
    }

    /// Open a sealed seed back into memory. Used by send/inbox/read once they land.
    #[allow(dead_code)]
    pub fn unwrap(&self, wrapped: &Wrapped) -> Result<[u8; 32], VaultError> {
        let pt = self
            .cipher
            .decrypt(XNonce::from_slice(&wrapped.nonce), wrapped.ct.as_slice())
            .map_err(|_| VaultError::Open)?;
        pt.try_into().map_err(|_| VaultError::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_round_trip() {
        let vault = Vault::new([7u8; 32]);
        let seed = [42u8; 32];
        let wrapped = vault.wrap(&seed).unwrap();
        assert_ne!(&wrapped.ct[..], &seed[..]); // actually encrypted
        assert_eq!(vault.unwrap(&wrapped).unwrap(), seed);
    }

    #[test]
    fn wrong_master_fails_to_open() {
        let a = Vault::new([1u8; 32]);
        let b = Vault::new([2u8; 32]);
        let wrapped = a.wrap(&[9u8; 32]).unwrap();
        assert!(b.unwrap(&wrapped).is_err());
    }
}
