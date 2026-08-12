//! Identity store (plan §6, `identities` + `vault_keys`).
//!
//! **P0 is in-memory** — enough to mint and hold identities within one process. Persistent Postgres
//! storage (so identities survive a restart and shard across replicas) is the next increment; the
//! shape here mirrors the planned tables so that swap is mechanical.

use crate::vault::Wrapped;
use std::collections::HashMap;
use std::sync::Mutex;

/// One hosted identity and its sealed key material.
// Most fields are written at mint time and read once the read paths land (send/inbox/read unwrap the
// seed; auth checks cap_hash). Allow dead_code until then rather than not storing what we'll need.
#[allow(dead_code)]
pub struct StoredIdentity {
    pub address: String,
    pub wrapped_seed: Wrapped,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    /// SHA-256 of the capability token; the plaintext token is returned to the caller only once.
    pub cap_hash: [u8; 32],
    pub label: Option<String>,
    pub created_at: u64,
}

#[derive(Default)]
pub struct Store {
    inner: Mutex<HashMap<String, StoredIdentity>>,
}

impl Store {
    pub fn new() -> Self {
        Store::default()
    }

    pub fn insert(&self, identity: StoredIdentity) {
        self.inner
            .lock()
            .expect("store lock")
            .insert(identity.address.clone(), identity);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("store lock").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::Wrapped;

    fn sample(addr: &str) -> StoredIdentity {
        StoredIdentity {
            address: addr.to_string(),
            wrapped_seed: Wrapped {
                nonce: [0; 24],
                ct: vec![1, 2, 3],
            },
            ed25519_pub: [0; 32],
            x25519_pub: [0; 32],
            cap_hash: [0; 32],
            label: None,
            created_at: 0,
        }
    }

    #[test]
    fn insert_counts() {
        let store = Store::new();
        assert_eq!(store.len(), 0);
        store.insert(sample("/k/aaa"));
        store.insert(sample("/k/bbb"));
        assert_eq!(store.len(), 2);
    }
}
