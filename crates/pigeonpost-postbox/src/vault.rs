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
    /// A key derived from the master key for keyed lookups (`keyed_hash`): stable across restarts,
    /// useless for opening anything sealed here.
    lookup_key: [u8; 32],
}

impl Vault {
    pub fn new(master: [u8; 32]) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"pigeonpost vault lookup key v1\0");
        h.update(master);
        Vault {
            cipher: XChaCha20Poly1305::new(Key::from_slice(&master)),
            lookup_key: h.finalize().into(),
        }
    }

    /// Seal arbitrary small bytes (an invitation's contact address, a link awaiting delivery).
    /// Output is `nonce || ciphertext`.
    pub fn seal_bytes(&self, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        let mut nonce = [0u8; 24];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
        let ct = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| VaultError::Seal)?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open what [`Vault::seal_bytes`] sealed.
    pub fn open_bytes(&self, sealed: &[u8]) -> Result<Vec<u8>, VaultError> {
        if sealed.len() < 24 {
            return Err(VaultError::Open);
        }
        let (nonce, ct) = sealed.split_at(24);
        self.cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|_| VaultError::Open)
    }

    /// A keyed HMAC-SHA256 of `data` under a purpose label, hex. Lets two equal values be matched
    /// without the table holding a dictionary-searchable hash (SDS §7.2 `contact_addr_hash`).
    pub fn keyed_hash(&self, purpose: &str, data: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(&self.lookup_key)
            .expect("HMAC accepts any key");
        mac.update(purpose.as_bytes());
        mac.update(b"\0");
        mac.update(data);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
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
    fn bytes_round_trip_and_keyed_hash_is_stable_and_keyed() {
        let vault = Vault::new([7u8; 32]);
        let sealed = vault.seal_bytes(b"jane@example.com").unwrap();
        assert!(!sealed.windows(4).any(|w| w == b"jane"));
        assert_eq!(vault.open_bytes(&sealed).unwrap(), b"jane@example.com");
        assert!(Vault::new([8u8; 32]).open_bytes(&sealed).is_err());
        assert!(vault.open_bytes(&sealed[..10]).is_err());
        let a = vault.keyed_hash("contact", b"x");
        assert_eq!(a, Vault::new([7u8; 32]).keyed_hash("contact", b"x"));
        assert_ne!(a, Vault::new([8u8; 32]).keyed_hash("contact", b"x"));
        assert_ne!(a, vault.keyed_hash("other", b"x"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn wrong_master_fails_to_open() {
        let a = Vault::new([1u8; 32]);
        let b = Vault::new([2u8; 32]);
        let wrapped = a.wrap(&[9u8; 32]).unwrap();
        assert!(b.unwrap(&wrapped).is_err());
    }
}
