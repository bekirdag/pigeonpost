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
use rusqlite::{params, Connection, OptionalExtension};
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
    created_at    INTEGER NOT NULL,
    account_id    TEXT
);
CREATE INDEX IF NOT EXISTS identities_by_cap ON identities(cap_hash);

CREATE TABLE IF NOT EXISTS messages (
    id         TEXT PRIMARY KEY,
    recipient  TEXT NOT NULL,
    sender     TEXT NOT NULL,
    wrap_blob  BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    read       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS messages_by_recipient ON messages(recipient);

CREATE TABLE IF NOT EXISTS accounts (
    id         TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS api_keys (
    key_hash   BLOB PRIMARY KEY,
    account_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
";

// Run after SCHEMA. The ALTER adds account_id to identity tables created before accounts existed; on
// a fresh DB (column already present) it errors "duplicate column name", which open() ignores. The
// index is created afterwards, once the column is guaranteed to exist.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE identities ADD COLUMN account_id TEXT",
    "CREATE INDEX IF NOT EXISTS identities_by_account ON identities(account_id)",
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("corrupt stored value: {0}")]
    Corrupt(&'static str),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store task failed")]
    Join,
}

/// One hosted identity and its sealed key material.
// x25519_pub, label, and created_at are persisted for future paths (device keys, display, metering)
// but not yet read by logic; allow until then.
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
    /// The owning account (API-key tier), or `None` for an anonymous ephemeral identity.
    pub account_id: Option<String>,
}

/// What a retention sweep removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapStats {
    pub identities: usize,
    pub messages: usize,
}

/// A stored, sealed message awaiting delivery to `recipient`. `wrap_blob` is a JSON-serialized
/// `pigeonpost_core::envelope::Wrap`.
pub struct Message {
    pub id: String,
    pub recipient: String,
    pub sender: String,
    pub wrap_blob: Vec<u8>,
    pub created_at: u64,
    pub read: bool,
}

/// Raw identity columns, before fixed-size arrays are validated.
struct IdRow {
    address: String,
    ed: Vec<u8>,
    x: Vec<u8>,
    nonce: Vec<u8>,
    ct: Vec<u8>,
    cap: Vec<u8>,
    label: Option<String>,
    created_at: i64,
    account_id: Option<String>,
}

fn arr<const N: usize>(v: Vec<u8>, what: &'static str) -> Result<[u8; N], StoreError> {
    v.try_into().map_err(|_| StoreError::Corrupt(what))
}

fn id_from_row(r: IdRow) -> Result<StoredIdentity, StoreError> {
    Ok(StoredIdentity {
        address: r.address,
        wrapped_seed: Wrapped {
            nonce: arr(r.nonce, "wrapped_nonce")?,
            ct: r.ct,
        },
        ed25519_pub: arr(r.ed, "ed25519_pub")?,
        x25519_pub: arr(r.x, "x25519_pub")?,
        cap_hash: arr(r.cap, "cap_hash")?,
        label: r.label,
        created_at: r.created_at as u64,
        account_id: r.account_id,
    })
}

const ID_COLS: &str = "address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id";

fn map_id_row(row: &rusqlite::Row) -> rusqlite::Result<IdRow> {
    Ok(IdRow {
        address: row.get(0)?,
        ed: row.get(1)?,
        x: row.get(2)?,
        nonce: row.get(3)?,
        ct: row.get(4)?,
        cap: row.get(5)?,
        label: row.get(6)?,
        created_at: row.get(7)?,
        account_id: row.get(8)?,
    })
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
            let c = Connection::open(path)?;
            // WAL lets the server and the separate reaper process share the file with a single
            // writer + concurrent readers; busy_timeout retries briefly instead of erroring on lock.
            c.pragma_update(None, "journal_mode", "WAL")?;
            c
        };
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(SCHEMA)?;
        for stmt in MIGRATIONS {
            if let Err(e) = conn.execute(stmt, []) {
                // "duplicate column name" means the migration already applied (fresh DB) — benign.
                if !e.to_string().contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }
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
                   (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.address,
                    &id.ed25519_pub[..],
                    &id.x25519_pub[..],
                    &id.wrapped_seed.nonce[..],
                    id.wrapped_seed.ct,
                    &id.cap_hash[..],
                    id.label,
                    id.created_at as i64,
                    id.account_id,
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

    /// Create an account with the given id and its first API key (hash).
    pub async fn create_account(
        &self,
        account_id: String,
        key_hash: [u8; 32],
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO accounts (id, created_at) VALUES (?1, ?2)",
                params![account_id, now as i64],
            )?;
            tx.execute(
                "INSERT INTO api_keys (key_hash, account_id, created_at) VALUES (?1, ?2, ?3)",
                params![&key_hash[..], account_id, now as i64],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The account an API key authenticates, if any.
    pub async fn account_for_key(&self, key_hash: [u8; 32]) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                "SELECT account_id FROM api_keys WHERE key_hash = ?1",
                params![&key_hash[..]],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Addresses (and labels) of every identity owned by an account, oldest first.
    pub async fn list_by_account(
        &self,
        account_id: String,
    ) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, Option<String>)>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT address, label FROM identities WHERE account_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Resolve one of an account's identities by address (ownership-checked).
    pub async fn get_in_account(
        &self,
        account_id: String,
        address: String,
    ) -> Result<Option<StoredIdentity>, StoreError> {
        self.query_one(
            format!("SELECT {ID_COLS} FROM identities WHERE address = ?1 AND account_id = ?2"),
            move |c, sql| {
                c.query_row(sql, params![address, account_id], map_id_row)
                    .optional()
            },
        )
        .await
    }

    /// Resolve the identity a capability token authenticates (bearer auth).
    pub async fn get_by_cap(
        &self,
        cap_hash: [u8; 32],
    ) -> Result<Option<StoredIdentity>, StoreError> {
        self.query_one(
            format!("SELECT {ID_COLS} FROM identities WHERE cap_hash = ?1"),
            move |c, sql| {
                c.query_row(sql, params![&cap_hash[..]], map_id_row)
                    .optional()
            },
        )
        .await
    }

    /// Resolve a hosted identity by its address (recipient lookup).
    pub async fn get(&self, address: String) -> Result<Option<StoredIdentity>, StoreError> {
        self.query_one(
            format!("SELECT {ID_COLS} FROM identities WHERE address = ?1"),
            move |c, sql| c.query_row(sql, params![address], map_id_row).optional(),
        )
        .await
    }

    /// Shared plumbing for the two single-identity lookups.
    async fn query_one<F>(&self, sql: String, run: F) -> Result<Option<StoredIdentity>, StoreError>
    where
        F: FnOnce(&Connection, &str) -> rusqlite::Result<Option<IdRow>> + Send + 'static,
    {
        let conn = self.conn.clone();
        let row = tokio::task::spawn_blocking(move || -> Result<Option<IdRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(run(&c, &sql)?)
        })
        .await
        .map_err(|_| StoreError::Join)??;
        row.map(id_from_row).transpose()
    }

    pub async fn enqueue(&self, m: Message) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO messages (id, recipient, sender, wrap_blob, created_at, read)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    m.id,
                    m.recipient,
                    m.sender,
                    m.wrap_blob,
                    m.created_at as i64,
                    m.read as i64
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Messages waiting for `recipient`, oldest first.
    pub async fn list_for(&self, recipient: String) -> Result<Vec<Message>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT id, recipient, sender, wrap_blob, created_at, read
                 FROM messages WHERE recipient = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![recipient], |row| {
                Ok(Message {
                    id: row.get(0)?,
                    recipient: row.get(1)?,
                    sender: row.get(2)?,
                    wrap_blob: row.get(3)?,
                    created_at: row.get::<_, i64>(4)? as u64,
                    read: row.get::<_, i64>(5)? != 0,
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Mark a message read, scoped to its recipient so one identity can't ack another's mail.
    pub async fn mark_read(&self, id: String, recipient: String) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let c = conn.lock().expect("store lock");
            let n = c.execute(
                "UPDATE messages SET read = 1 WHERE id = ?1 AND recipient = ?2",
                params![id, recipient],
            )?;
            Ok(n > 0)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Retention sweep: drop everything older than `cutoff` (unix seconds) — messages past their
    /// window, identities past theirs, and any message addressed to or from an expired identity.
    /// Runs in one transaction so a crash can't leave a half-swept state.
    pub async fn reap(&self, cutoff: u64) -> Result<ReapStats, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<ReapStats, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let messages = tx.execute(
                "DELETE FROM messages
                 WHERE created_at < ?1
                    OR recipient IN (SELECT address FROM identities WHERE created_at < ?1)
                    OR sender    IN (SELECT address FROM identities WHERE created_at < ?1)",
                params![cutoff as i64],
            )?;
            let identities = tx.execute(
                "DELETE FROM identities WHERE created_at < ?1",
                params![cutoff as i64],
            )?;
            tx.commit()?;
            Ok(ReapStats {
                identities,
                messages,
            })
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
            account_id: None,
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

    #[tokio::test]
    async fn message_round_trip_seals_stores_and_opens() {
        use pigeonpost_core::{envelope, Identity};
        let store = Store::open(":memory:").unwrap();
        let alice = Identity::generate();
        let bob = Identity::generate();
        let bob_addr = bob.address().as_str().to_string();

        let wrap = envelope::wrap(&alice, &bob.verifying_key(), "hello bob", 1000).unwrap();
        store
            .enqueue(Message {
                id: "m1".into(),
                recipient: bob_addr.clone(),
                sender: alice.address().as_str().to_string(),
                wrap_blob: serde_json::to_vec(&wrap).unwrap(),
                created_at: 1000,
                read: false,
            })
            .await
            .unwrap();

        let msgs = store.list_for(bob_addr.clone()).await.unwrap();
        assert_eq!(msgs.len(), 1);
        let stored: envelope::Wrap = serde_json::from_slice(&msgs[0].wrap_blob).unwrap();
        let (from, body) = envelope::open(&bob, &stored).unwrap();
        assert_eq!(body.as_str(), "hello bob");
        assert_eq!(from, alice.verifying_key());

        // ack is scoped to the recipient
        assert!(store.mark_read("m1".into(), bob_addr).await.unwrap());
        assert!(!store
            .mark_read("m1".into(), "/k/other".into())
            .await
            .unwrap());
    }

    fn msg(id: &str, recipient: &str, created_at: u64) -> Message {
        Message {
            id: id.into(),
            recipient: recipient.into(),
            sender: "/k/sender".into(),
            wrap_blob: vec![1, 2, 3],
            created_at,
            read: false,
        }
    }

    #[tokio::test]
    async fn account_api_key_and_ownership() {
        let store = Store::open(":memory:").unwrap();
        store.create_account("acct_1".into(), [7; 32], 0).await.unwrap();
        assert_eq!(store.account_for_key([7; 32]).await.unwrap().as_deref(), Some("acct_1"));
        assert_eq!(store.account_for_key([9; 32]).await.unwrap(), None);

        let mut a = sample("/k/mine");
        a.account_id = Some("acct_1".into());
        store.insert(a).await.unwrap();
        store.insert(sample("/k/anon")).await.unwrap(); // no account

        let owned = store.list_by_account("acct_1".into()).await.unwrap();
        assert_eq!(owned, vec![("/k/mine".to_string(), Some("repo:test".to_string()))]);
        assert!(store.get_in_account("acct_1".into(), "/k/mine".into()).await.unwrap().is_some());
        assert!(store.get_in_account("acct_1".into(), "/k/anon".into()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reap_removes_expired_identities_and_messages() {
        let store = Store::open(":memory:").unwrap();
        let mut fresh = sample("/k/fresh");
        fresh.created_at = 10_000;
        let mut old = sample("/k/old");
        old.created_at = 0;
        store.insert(fresh).await.unwrap();
        store.insert(old).await.unwrap();

        store.enqueue(msg("new", "/k/fresh", 10_000)).await.unwrap(); // survives
        store.enqueue(msg("stale", "/k/fresh", 0)).await.unwrap(); // past its window
        store
            .enqueue(msg("orphan", "/k/old", 10_000))
            .await
            .unwrap(); // recipient expired

        let stats = store.reap(5_000).await.unwrap();
        assert_eq!(stats.identities, 1); // /k/old
        assert_eq!(stats.messages, 2); // "stale" + "orphan"

        assert_eq!(store.count().await.unwrap(), 1); // /k/fresh remains
        let remaining = store.list_for("/k/fresh".into()).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "new");
    }
}
