//! Identity store (plan §6, `identities` + `vault_keys`).
//!
//! P0 persistence is **SQLite** (bundled, embedded — no external DB, and testable in CI), matching
//! the registry crate. Postgres is the P2 swap; the columns here mirror the planned tables so the
//! move is mechanical. The sealed key material is stored inline for now (one table); it splits into
//! `vault_keys` when multi-device device-sets land.
//!
//! `rusqlite` is synchronous, so writes/reads run on `spawn_blocking` to avoid stalling the async
//! runtime; a single connection behind a mutex is ample at P0 volume.

use crate::vault::Wrapped;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
    address       TEXT PRIMARY KEY,
    ed25519_pub   BLOB NOT NULL,
    x25519_pub    BLOB NOT NULL,
    wrapped_nonce BLOB NOT NULL,
    wrapped_ct    BLOB NOT NULL,
    cap_hash      BLOB NOT NULL,
    label         TEXT,
    created_at    INTEGER NOT NULL
);
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store task failed")]
    Join,
}

/// One hosted identity and its sealed key material.
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

/// SQLite-backed identity store.
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open (creating if needed) the database at `path`, or `":memory:"` for tests. Applies the
    /// schema idempotently.
    pub fn open(path: &str) -> Result<Self, StoreError> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        conn.execute_batch(SCHEMA)?;
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert(&self, id: StoredIdentity) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO identities
                   (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id.address,
                    &id.ed25519_pub[..],
                    &id.x25519_pub[..],
                    &id.wrapped_seed.nonce[..],
                    id.wrapped_seed.ct,
                    &id.cap_hash[..],
                    id.label,
                    id.created_at as i64,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn count(&self) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            let n: i64 = c.query_row("SELECT COUNT(*) FROM identities", [], |r| r.get(0))?;
            Ok(n as usize)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            label: Some("repo:test".into()),
            created_at: 0,
        }
    }

    #[tokio::test]
    async fn insert_and_count() {
        let store = Store::open(":memory:").unwrap();
        assert_eq!(store.count().await.unwrap(), 0);
        store.insert(sample("/k/aaa")).await.unwrap();
        store.insert(sample("/k/bbb")).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn duplicate_address_is_rejected() {
        let store = Store::open(":memory:").unwrap();
        store.insert(sample("/k/dup")).await.unwrap();
        assert!(store.insert(sample("/k/dup")).await.is_err()); // PRIMARY KEY conflict
    }
}
