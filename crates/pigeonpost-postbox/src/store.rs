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
    created_at INTEGER NOT NULL,
    oidc_sub   TEXT
);
CREATE TABLE IF NOT EXISTS api_keys (
    key_hash   BLOB PRIMARY KEY,
    account_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    id         TEXT,
    prefix     TEXT
);

-- One row per *unauthenticated* mint (anonymous identity or account creation), keyed by the
-- caller's IP. Feeds the per-IP rate limit that replaces the old account-quota-only ceiling, and
-- is the raw signal the reputation work scores mint sources against.
CREATE TABLE IF NOT EXISTS mint_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ip         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    address    TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS mint_events_by_ip ON mint_events(ip, created_at);

-- Who an inbox knows, and on what terms. Three axes, deliberately not collapsed:
--   admission     — may this peer's mail be delivered at all  (allow | block)
--   autonomy      — may my agent act on it without a human    (review | auto)
--   allowed_verbs — to do which specific things               (JSON array; NULL/[] = none)
-- `allow` is the admission default because the postbox has always delivered to anyone; `review`
-- is the autonomy default because trusting a sender's identity is not trusting their instructions;
-- and the verb list starts empty because `auto` with no verbs grants nothing, which is the right
-- resting state for a switch this sharp.
CREATE TABLE IF NOT EXISTS contacts (
    owner         TEXT NOT NULL,
    peer          TEXT NOT NULL,
    alias         TEXT,
    admission     TEXT NOT NULL DEFAULT 'allow',
    autonomy      TEXT NOT NULL DEFAULT 'review',
    allowed_verbs TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (owner, peer)
);

-- Which account owns a purchased handle namespace, cached from the registry.
--
-- The registry stays the public record of who bought `/bekir`; this is the operational binding the
-- postbox needs to answer \"may this caller mint under it\". `expires_at` keeps it a cache rather
-- than a second source of truth — past it, the answer is re-fetched instead of assumed.
CREATE TABLE IF NOT EXISTS namespaces (
    namespace   TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL,
    source      TEXT NOT NULL,
    verified_at INTEGER NOT NULL,
    expires_at  INTEGER
);
CREATE INDEX IF NOT EXISTS namespaces_by_account ON namespaces(account_id);

-- Upheld spam reports, keyed by message so a recipient re-reporting the same message cannot
-- charge its sender twice. `reporter` is kept so a reporter that turns out to be the abuser can
-- have its reports reconsidered.
CREATE TABLE IF NOT EXISTS spam_reports (
    message_id TEXT PRIMARY KEY,
    reporter   TEXT NOT NULL,
    sender     TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS spam_reports_by_sender ON spam_reports(sender);

-- What a subject has earned, as opposed to what a human granted it. `subject` is either a `/k/`
-- address (kind='sender') or an IP (kind='mint_ip'); one table because the arithmetic is the same
-- and only the lookup key differs.
CREATE TABLE IF NOT EXISTS reputation (
    subject    TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    reports    INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

-- Workspace context for one mailbox: which repo it works on, what its job is, which machine and
-- path it lives at. Stored as opaque ciphertext — the postbox holds the bytes and the salt, never
-- a key, so an operator with database access learns nothing but that context exists and its size.
CREATE TABLE IF NOT EXISTS workspace_context (
    address    TEXT PRIMARY KEY,
    nonce      BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    kdf_salt   BLOB NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Per-inbox defaults for senders with no contact row. Absent = the defaults in `InboxPolicy`.
CREATE TABLE IF NOT EXISTS inbox_policy (
    address           TEXT PRIMARY KEY,
    accept_all        INTEGER NOT NULL DEFAULT 1,
    auto_accept_known INTEGER NOT NULL DEFAULT 0,
    updated_at        INTEGER NOT NULL
);
";

// Run after SCHEMA. ALTERs add columns to tables created before those features existed; on a fresh
// DB (column already present) they error "duplicate column name", which open() ignores. Indexes come
// afterwards, once their columns are guaranteed to exist. (SQLite unique indexes treat NULLs as
// distinct, so many API-key accounts can share a NULL oidc_sub.)
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE identities ADD COLUMN account_id TEXT",
    "CREATE INDEX IF NOT EXISTS identities_by_account ON identities(account_id)",
    "ALTER TABLE accounts ADD COLUMN oidc_sub TEXT",
    "CREATE UNIQUE INDEX IF NOT EXISTS accounts_by_sub ON accounts(oidc_sub)",
    "ALTER TABLE api_keys ADD COLUMN id TEXT",
    "ALTER TABLE api_keys ADD COLUMN prefix TEXT",
    "UPDATE api_keys SET id = lower(hex(randomblob(8))) WHERE id IS NULL",
    "CREATE INDEX IF NOT EXISTS api_keys_by_account ON api_keys(account_id)",
    // A handle mailbox is one name bound to one key, so the index is unique. NULL for every `/k/`
    // mailbox, and SQLite treats NULLs as distinct, so unlimited anonymous mints still coexist.
    "ALTER TABLE identities ADD COLUMN handle TEXT",
    "CREATE UNIQUE INDEX IF NOT EXISTS identities_by_handle ON identities(handle)",
    // Contacts written before the scoped request envelope existed get a NULL verb list, i.e. no
    // grants. Any `auto` they already carried therefore stops meaning "act on any prose from this
    // sender" the moment this ships — which is the migration we want, not one to paper over.
    "ALTER TABLE contacts ADD COLUMN allowed_verbs TEXT",
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
    /// The handle this mailbox answers to, e.g. `/bekir/agent1`, or `None` for a bare `/k/` one.
    pub handle: Option<String>,
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

/// A new API key's storable fields: the secret hash, a revocable id, and a display prefix.
pub struct NewKey {
    pub key_hash: [u8; 32],
    pub id: String,
    pub prefix: String,
}

fn insert_api_key(
    conn: &Connection,
    account_id: &str,
    key: &NewKey,
    now: u64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO api_keys (key_hash, account_id, created_at, id, prefix) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![&key.key_hash[..], account_id, now as i64, key.id, key.prefix],
    )?;
    Ok(())
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
    handle: Option<String>,
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
        handle: r.handle,
    })
}

/// A peer an inbox knows, and the terms it knows them on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub owner: String,
    pub peer: String,
    pub alias: Option<String>,
    /// `allow` | `block` — whether their mail is delivered at all.
    pub admission: String,
    /// `review` | `auto` — whether the recipient's agent may act without a human.
    pub autonomy: String,
    /// Which request verbs this peer may have acted on. Empty — the default — means `autonomy`
    /// alone grants nothing, because every message still has to name a verb on this list.
    pub allowed_verbs: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A partial contact write. `None` means "leave whatever is stored".
pub struct ContactUpdate {
    pub owner: String,
    pub peer: String,
    pub alias: Option<String>,
    pub admission: Option<String>,
    pub autonomy: Option<String>,
    /// `Some(vec![])` clears every grant; `None` leaves the stored list alone.
    pub allowed_verbs: Option<Vec<String>>,
    pub now: u64,
}

/// What an inbox does about senders it has no contact row for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxPolicy {
    /// Accept mail from strangers. On by default — the postbox has always been open, and closing
    /// it by default would silently break every existing mailbox.
    pub accept_all: bool,
    /// Treat *known* contacts as `auto` even when their row says `review`. Off by default, and
    /// only a human can turn it on.
    pub auto_accept_known: bool,
}

impl Default for InboxPolicy {
    fn default() -> Self {
        InboxPolicy {
            accept_all: true,
            auto_accept_known: false,
        }
    }
}

/// The namespace-wide contact that would cover `peer`, e.g. `/bekir/*` for `/bekir/agent1`.
///
/// `None` for a `/k/` address: those have no namespace to belong to, so nothing but an exact row
/// can ever speak for them.
/// Whether a namespace had room for one more mailbox at the moment of the write.
#[derive(Debug, PartialEq, Eq)]
pub enum QuotaOutcome {
    Inserted,
    Full,
}

/// What happened when an existing mailbox asked for a name.
#[derive(Debug, PartialEq, Eq)]
pub enum BindOutcome {
    Bound,
    /// No mailbox at that address on this postbox.
    NoSuchMailbox,
    /// The mailbox exists but belongs to another account.
    NotYours,
    /// It already answers to this handle; carries the current one so the caller can say which.
    AlreadyNamed(String),
    /// Another mailbox got that handle first.
    Taken,
    /// The namespace is at its ceiling.
    Full,
}

pub fn namespace_wildcard(peer: &str) -> Option<String> {
    let rest = peer.strip_prefix('/')?;
    let (namespace, name) = rest.split_once('/')?;
    if namespace.is_empty() || name.is_empty() || namespace == "k" {
        return None;
    }
    Some(format!("/{namespace}/*"))
}

/// The contact columns, in the order `map_contact_row` reads them.
const CONTACT_COLS: &str =
    "owner, peer, alias, admission, autonomy, allowed_verbs, created_at, updated_at";

fn map_contact_row(row: &rusqlite::Row) -> rusqlite::Result<Contact> {
    Ok(Contact {
        owner: row.get(0)?,
        peer: row.get(1)?,
        alias: row.get(2)?,
        admission: row.get(3)?,
        autonomy: row.get(4)?,
        // Unreadable JSON degrades to "no grants" rather than erroring the whole read: a corrupt
        // verb list must never be the reason an inbox can't be listed, and failing it closed costs
        // the owner one re-grant.
        allowed_verbs: row
            .get::<_, Option<String>>(5)?
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default(),
        created_at: row.get::<_, i64>(6)? as u64,
        updated_at: row.get::<_, i64>(7)? as u64,
    })
}

const ID_COLS: &str = "address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id, handle";

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
        handle: row.get(9)?,
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
                   (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id, handle)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    id.handle,
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

    /// How many identities an account holds (identity quota check).
    pub async fn count_for_account(&self, account_id: String) -> Result<usize, StoreError> {
        self.count_where(
            "SELECT COUNT(*) FROM identities WHERE account_id = ?1",
            account_id,
        )
        .await
    }

    /// How many messages an inbox holds (inbox quota check).
    pub async fn inbox_count(&self, recipient: String) -> Result<usize, StoreError> {
        self.count_where(
            "SELECT COUNT(*) FROM messages WHERE recipient = ?1",
            recipient,
        )
        .await
    }

    /// Messages this inbox has not acked. This — not the total — is what a long poll waits on:
    /// an inbox holding only already-read mail is quiet, and waiting on the total would make a
    /// caller with one un-acked message spin instead of wait.
    pub async fn unread_count(&self, recipient: String) -> Result<usize, StoreError> {
        self.count_where(
            "SELECT COUNT(*) FROM messages WHERE recipient = ?1 AND read = 0",
            recipient,
        )
        .await
    }

    /// Which account owns a purchased namespace, if the cached binding is still fresh.
    ///
    /// Returns `None` both for "nobody owns it" and for "the cache has expired", because the
    /// caller's next move is the same either way: ask the registry rather than assume.
    pub async fn namespace_owner(
        &self,
        namespace: String,
        now: u64,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            c.query_row(
                "SELECT account_id FROM namespaces
                  WHERE namespace = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
                params![namespace, now as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record or refresh a namespace binding.
    ///
    /// Only tests call this today: nothing yet syncs ownership from the registry, which is the
    /// remaining piece of Phase 1. Left public and unused rather than deleted because the mint
    /// path already reads what it writes, and the read half is worthless without it.
    #[allow(dead_code)]
    pub async fn set_namespace_owner(
        &self,
        namespace: String,
        account_id: String,
        source: &'static str,
        now: u64,
        expires_at: Option<u64>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO namespaces (namespace, account_id, source, verified_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(namespace) DO UPDATE SET
                     account_id = ?2, source = ?3, verified_at = ?4, expires_at = ?5",
                params![
                    namespace,
                    account_id,
                    source,
                    now as i64,
                    expires_at.map(|v| v as i64)
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// How many mailboxes already exist under one namespace — the per-handle ceiling's input.
    ///
    /// Counted from `identities` rather than a running total, so it cannot drift away from the
    /// mailboxes that actually exist after deletions.
    pub async fn count_for_namespace(&self, namespace: String) -> Result<usize, StoreError> {
        let prefix = format!("{namespace}/");
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM identities WHERE handle LIKE ?1 || '%'",
                params![prefix],
                |r| r.get(0),
            )?;
            Ok(n as usize)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Insert a mailbox that carries a handle, refusing it if the namespace is already at `max`.
    ///
    /// The count and the insert share one lock and one transaction. Checking the quota in a
    /// separate call would let two mints racing at the boundary both see a free slot and both
    /// take it — the admission race `docs/architecture.md` describes for registry claims, which
    /// the registry avoids by counting inside the writer transaction. Same discipline here.
    pub async fn insert_under_namespace(
        &self,
        id: StoredIdentity,
        namespace: String,
        max: usize,
    ) -> Result<QuotaOutcome, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<QuotaOutcome, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let prefix = format!("{namespace}/");
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM identities WHERE handle LIKE ?1 || '%'",
                params![prefix],
                |r| r.get(0),
            )?;
            if held as usize >= max {
                return Ok(QuotaOutcome::Full);
            }
            tx.execute(
                "INSERT INTO identities
                   (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id, handle)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    id.handle,
                ],
            )?;
            tx.commit()?;
            Ok(QuotaOutcome::Inserted)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Give an existing `/k/` mailbox a handle.
    ///
    /// The mailbox someone already runs is the one their agent is configured against, so the way
    /// into a namespace cannot be "mint a new one and move" — that would cost them their address,
    /// their contacts' trust entries, and their mail. This binds the name to the mailbox they have.
    ///
    /// Deliberately refuses a mailbox that already carries a handle: rebinding means deciding what
    /// happens to everyone who trusts the old name, which is a different feature.
    ///
    /// One transaction, for the same reason as `insert_under_namespace`.
    pub async fn bind_handle(
        &self,
        address: String,
        handle: String,
        account: String,
        namespace: String,
        max: usize,
    ) -> Result<BindOutcome, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<BindOutcome, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

            let existing: Option<(Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT account_id, handle FROM identities WHERE address = ?1",
                    params![address],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((owner, current)) = existing else {
                return Ok(BindOutcome::NoSuchMailbox);
            };
            // Same refusal for "not yours" as for "no such mailbox" would be kinder to probing,
            // but the caller already had to authenticate as an account to get here, and telling
            // an owner apart from a stranger matters more for a command someone runs by hand.
            if owner.as_deref() != Some(account.as_str()) {
                return Ok(BindOutcome::NotYours);
            }
            if let Some(current) = current {
                return Ok(BindOutcome::AlreadyNamed(current));
            }

            let prefix = format!("{namespace}/");
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM identities WHERE handle LIKE ?1 || '%'",
                params![prefix],
                |r| r.get(0),
            )?;
            if held as usize >= max {
                return Ok(BindOutcome::Full);
            }

            // The unique index on `handle` is what actually decides the winner between two callers
            // asking for the same name; this turns its error into an answer rather than a 500.
            match tx.execute(
                "UPDATE identities SET handle = ?1 WHERE address = ?2",
                params![handle, address],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    return Ok(BindOutcome::Taken);
                }
                Err(e) => return Err(e.into()),
            }
            tx.commit()?;
            Ok(BindOutcome::Bound)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Resolve a handle to its hosted mailbox.
    pub async fn get_by_handle(
        &self,
        handle: String,
    ) -> Result<Option<StoredIdentity>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<StoredIdentity>, StoreError> {
            let c = conn.lock().expect("store lock");
            let row = c
                .query_row(
                    &format!("SELECT {ID_COLS} FROM identities WHERE handle = ?1"),
                    params![handle],
                    map_id_row,
                )
                .optional()?;
            row.map(id_from_row).transpose()
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// How many upheld reports a subject carries. Reports, not a score: the score is derived from
    /// this plus the subject's tier, so a mailbox that later gains an account is re-scored rather
    /// than stuck with whatever number was written when it was anonymous.
    pub async fn reports_against(&self, subject: String) -> Result<u32, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<u32, StoreError> {
            let c = conn.lock().expect("store lock");
            let n: Option<i64> = c
                .query_row(
                    "SELECT reports FROM reputation WHERE subject = ?1",
                    params![subject],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(n.unwrap_or(0).max(0) as u32)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record one spam report and charge it to the sender and to the IP that minted them.
    ///
    /// Returns `false` when this message was already reported — the message id is the primary key
    /// precisely so a recipient cannot charge the same message twice by asking twice.
    pub async fn record_spam_report(
        &self,
        message_id: String,
        reporter: String,
        sender: String,
        now: u64,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO spam_reports (message_id, reporter, sender, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![message_id, reporter, sender, now as i64],
            )?;
            if inserted == 0 {
                return Ok(false);
            }

            let charge = |subject: &str, kind: &str| -> rusqlite::Result<()> {
                tx.execute(
                    "INSERT INTO reputation (subject, kind, reports, updated_at)
                     VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT(subject) DO UPDATE SET
                         reports = reports + 1, updated_at = ?3",
                    params![subject, kind, now as i64],
                )?;
                Ok(())
            };
            charge(&sender, "sender")?;

            // Charge the source too, or a flood just burns the reported inbox and mints another.
            let minting_ip: Option<String> = tx
                .query_row(
                    "SELECT ip FROM mint_events WHERE address = ?1 ORDER BY id LIMIT 1",
                    params![sender],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(ip) = minting_ip {
                charge(&ip, "mint_ip")?;
            }

            tx.commit()?;
            Ok(true)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The sender of one message in `recipient`'s inbox. `None` when it isn't theirs to report.
    pub async fn message_sender(
        &self,
        message_id: String,
        recipient: String,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            c.query_row(
                "SELECT sender FROM messages WHERE id = ?1 AND recipient = ?2",
                params![message_id, recipient],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Messages `sender` has put in `recipient`'s inbox since `since` — the stranger throttle's
    /// input. Counts delivered messages, so acking or reading does not reopen the allowance.
    pub async fn messages_between_since(
        &self,
        sender: String,
        recipient: String,
        since: u64,
    ) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM messages
                  WHERE sender = ?1 AND recipient = ?2 AND created_at >= ?3",
                params![sender, recipient, since as i64],
                |r| r.get(0),
            )?;
            Ok(n as usize)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record one unauthenticated mint against the caller's IP. Best-effort accounting: a failure
    /// here must never fail the mint the caller already earned with a proof-of-work.
    pub async fn record_mint(
        &self,
        ip: String,
        kind: &'static str,
        address: Option<String>,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO mint_events (ip, kind, address, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![ip, kind, address, now as i64],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Mints from one IP: `(within the window starting at `since`, lifetime)`. Both come from one
    /// query so a burst can't slip between two reads.
    pub async fn mint_counts(&self, ip: String, since: u64) -> Result<(usize, usize), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), StoreError> {
            let c = conn.lock().expect("store lock");
            let (recent, total): (i64, i64) = c.query_row(
                "SELECT COALESCE(SUM(created_at >= ?2), 0), COUNT(*)
                   FROM mint_events WHERE ip = ?1",
                params![ip, since as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((recent as usize, total as usize))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// When the oldest mint inside the current window happened — the point at which a
    /// rate-limited caller regains a slot. `None` when the IP has no mints in the window.
    pub async fn oldest_mint_in_window(
        &self,
        ip: String,
        since: u64,
    ) -> Result<Option<u64>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<u64>, StoreError> {
            let c = conn.lock().expect("store lock");
            let at: Option<i64> = c.query_row(
                "SELECT MIN(created_at) FROM mint_events WHERE ip = ?1 AND created_at >= ?2",
                params![ip, since as i64],
                |r| r.get(0),
            )?;
            Ok(at.map(|v| v as u64))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Upsert one contact. Fields left as `None` keep their stored value, so a caller may change
    /// one axis without knowing the others.
    pub async fn upsert_contact(&self, c: ContactUpdate) -> Result<Contact, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Contact, StoreError> {
            let conn = conn.lock().expect("store lock");
            // Serialized here rather than in the caller so the column's encoding stays the store's
            // business. An empty grant is stored as `[]`, distinct from NULL only in intent.
            let verbs = c
                .allowed_verbs
                .as_ref()
                .map(|v| serde_json::to_string(v).expect("Vec<String> always serializes"));
            conn.execute(
                "INSERT INTO contacts
                     (owner, peer, alias, admission, autonomy, allowed_verbs, created_at, updated_at)
                 VALUES (?1, ?2, ?3, COALESCE(?4, 'allow'), COALESCE(?5, 'review'), ?6, ?7, ?7)
                 ON CONFLICT(owner, peer) DO UPDATE SET
                     alias         = COALESCE(?3, alias),
                     admission     = COALESCE(?4, admission),
                     autonomy      = COALESCE(?5, autonomy),
                     allowed_verbs = COALESCE(?6, allowed_verbs),
                     updated_at    = ?7",
                params![
                    c.owner,
                    c.peer,
                    c.alias,
                    c.admission,
                    c.autonomy,
                    verbs,
                    c.now as i64
                ],
            )?;
            conn.query_row(
                &format!("SELECT {CONTACT_COLS} FROM contacts WHERE owner = ?1 AND peer = ?2"),
                params![c.owner, c.peer],
                map_contact_row,
            )
            .map_err(Into::into)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Store one mailbox's encrypted workspace context, replacing any previous one.
    pub async fn put_workspace(
        &self,
        address: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        kdf_salt: Vec<u8>,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO workspace_context (address, nonce, ciphertext, kdf_salt, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(address) DO UPDATE SET
                     nonce = ?2, ciphertext = ?3, kdf_salt = ?4, updated_at = ?5",
                params![address, nonce, ciphertext, kdf_salt, now as i64],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Fetch one mailbox's encrypted workspace context: `(nonce, ciphertext, salt, updated_at)`.
    #[allow(clippy::type_complexity)]
    pub async fn workspace(
        &self,
        address: String,
    ) -> Result<Option<(Vec<u8>, Vec<u8>, Vec<u8>, u64)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> Result<Option<(Vec<u8>, Vec<u8>, Vec<u8>, u64)>, StoreError> {
                let c = conn.lock().expect("store lock");
                c.query_row(
                    "SELECT nonce, ciphertext, kdf_salt, updated_at
                       FROM workspace_context WHERE address = ?1",
                    params![address],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as u64)),
                )
                .optional()
                .map_err(Into::into)
            },
        )
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The contact governing `peer`: their own row if they have one, otherwise their namespace's.
    ///
    /// Most specific wins, decided in SQL so the two lookups cannot disagree. An exact row for
    /// `/bekir/agent9` therefore still outranks `/bekir/*`, which is what lets one bad agent be
    /// blocked without disowning the whole fleet.
    pub async fn governing_contact(
        &self,
        owner: String,
        peer: String,
    ) -> Result<Option<Contact>, StoreError> {
        let wildcard = namespace_wildcard(&peer);
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Contact>, StoreError> {
            let conn = conn.lock().expect("store lock");
            conn.query_row(
                &format!(
                    "SELECT {CONTACT_COLS} FROM contacts
                      WHERE owner = ?1 AND (peer = ?2 OR (?3 IS NOT NULL AND peer = ?3))
                      ORDER BY CASE WHEN peer = ?2 THEN 0 ELSE 1 END
                      LIMIT 1"
                ),
                params![owner, peer, wildcard],
                map_contact_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// One contact row, or `None` when the peer is a stranger.
    pub async fn contact(
        &self,
        owner: String,
        peer: String,
    ) -> Result<Option<Contact>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Contact>, StoreError> {
            let conn = conn.lock().expect("store lock");
            conn.query_row(
                &format!("SELECT {CONTACT_COLS} FROM contacts WHERE owner = ?1 AND peer = ?2"),
                params![owner, peer],
                map_contact_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn list_contacts(&self, owner: String) -> Result<Vec<Contact>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Contact>, StoreError> {
            let conn = conn.lock().expect("store lock");
            let mut stmt = conn.prepare(&format!(
                "SELECT {CONTACT_COLS} FROM contacts WHERE owner = ?1 ORDER BY created_at"
            ))?;
            let rows = stmt.query_map(params![owner], map_contact_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Remove a contact, returning whether one was there. The peer reverts to stranger terms.
    pub async fn delete_contact(&self, owner: String, peer: String) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let conn = conn.lock().expect("store lock");
            let n = conn.execute(
                "DELETE FROM contacts WHERE owner = ?1 AND peer = ?2",
                params![owner, peer],
            )?;
            Ok(n > 0)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// An inbox's stranger-defaults, or [`InboxPolicy::default`] when it has never set any.
    pub async fn inbox_policy(&self, address: String) -> Result<InboxPolicy, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<InboxPolicy, StoreError> {
            let conn = conn.lock().expect("store lock");
            let found = conn
                .query_row(
                    "SELECT accept_all, auto_accept_known FROM inbox_policy WHERE address = ?1",
                    params![address],
                    |r| {
                        Ok(InboxPolicy {
                            accept_all: r.get::<_, i64>(0)? != 0,
                            auto_accept_known: r.get::<_, i64>(1)? != 0,
                        })
                    },
                )
                .optional()?;
            Ok(found.unwrap_or_default())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Set an inbox's stranger-defaults. `None` fields keep their stored value.
    pub async fn set_inbox_policy(
        &self,
        address: String,
        accept_all: Option<bool>,
        auto_accept_known: Option<bool>,
        now: u64,
    ) -> Result<InboxPolicy, StoreError> {
        let current = self.inbox_policy(address.clone()).await?;
        let next = InboxPolicy {
            accept_all: accept_all.unwrap_or(current.accept_all),
            auto_accept_known: auto_accept_known.unwrap_or(current.auto_accept_known),
        };
        let conn = self.conn.clone();
        let stored = next;
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let conn = conn.lock().expect("store lock");
            conn.execute(
                "INSERT INTO inbox_policy (address, accept_all, auto_accept_known, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(address) DO UPDATE SET
                     accept_all = ?2, auto_accept_known = ?3, updated_at = ?4",
                params![
                    address,
                    stored.accept_all as i64,
                    stored.auto_accept_known as i64,
                    now as i64
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)??;
        Ok(next)
    }

    async fn count_where(&self, sql: &'static str, arg: String) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            let n: i64 = c.query_row(sql, params![arg], |r| r.get(0))?;
            Ok(n as usize)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Create an account with the given id and its first API key (hash).
    pub async fn create_account(
        &self,
        account_id: String,
        key: NewKey,
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
            insert_api_key(&tx, &account_id, &key, now)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Get (or create) the account for an OIDC subject. `candidate_id` is used only when creating.
    pub async fn account_for_sub(
        &self,
        sub: String,
        candidate_id: String,
        now: u64,
    ) -> Result<String, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<String, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM accounts WHERE oidc_sub = ?1",
                    params![sub],
                    |r| r.get(0),
                )
                .optional()?;
            let id = match existing {
                Some(id) => id,
                None => {
                    tx.execute(
                        "INSERT INTO accounts (id, created_at, oidc_sub) VALUES (?1, ?2, ?3)",
                        params![candidate_id, now as i64, sub],
                    )?;
                    candidate_id
                }
            };
            tx.commit()?;
            Ok(id)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Issue an additional API key for an existing account.
    pub async fn add_api_key(
        &self,
        account_id: String,
        key: NewKey,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            insert_api_key(&c, &account_id, &key, now)?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// List an account's API keys (id, display prefix, created_at) — never the secret.
    pub async fn list_keys(
        &self,
        account_id: String,
    ) -> Result<Vec<(String, String, u64)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, String, u64)>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT COALESCE(id, ''), COALESCE(prefix, ''), created_at
                 FROM api_keys WHERE account_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![account_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
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

    /// Revoke one of an account's API keys by id (ownership-scoped).
    pub async fn revoke_key(&self, account_id: String, id: String) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let c = conn.lock().expect("store lock");
            let n = c.execute(
                "DELETE FROM api_keys WHERE account_id = ?1 AND id = ?2",
                params![account_id, id],
            )?;
            Ok(n > 0)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Delete one of an account's identities and everything in/for its inbox (ownership-scoped).
    /// Delete one identity and everything it owns.
    ///
    /// `account_id` scopes the delete to an account's own inboxes. `None` means the caller proved
    /// control of the mailbox itself with its capability token — the only credential an
    /// anonymously minted `/k/` address ever has, and so the only way one can be destroyed.
    pub async fn delete_identity(
        &self,
        account_id: Option<String>,
        address: String,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let removed = match &account_id {
                Some(account) => tx.execute(
                    "DELETE FROM identities WHERE address = ?1 AND account_id = ?2",
                    params![address, account],
                )?,
                None => tx.execute(
                    "DELETE FROM identities WHERE address = ?1",
                    params![address],
                )?,
            };
            if removed > 0 {
                tx.execute(
                    "DELETE FROM messages WHERE recipient = ?1 OR sender = ?1",
                    params![address],
                )?;
                // The inbox's own trust state goes with it. Rows where this address is somebody
                // *else's* peer are left alone: that is their contact book, not ours to edit, and
                // an address is derived from a key so a deleted one can never be re-minted and
                // inherit the terms.
                tx.execute("DELETE FROM contacts WHERE owner = ?1", params![address])?;
                tx.execute(
                    "DELETE FROM inbox_policy WHERE address = ?1",
                    params![address],
                )?;
                // Workspace context describes a repo, a machine and a filesystem path. Leaving it
                // behind after the mailbox is gone would be keeping reconnaissance data about a
                // person who asked to be deleted.
                tx.execute(
                    "DELETE FROM workspace_context WHERE address = ?1",
                    params![address],
                )?;
            }
            tx.commit()?;
            Ok(removed > 0)
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
            handle: None,
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

    #[tokio::test]
    async fn mint_events_count_per_ip_and_window() {
        let store = Store::open(":memory:").unwrap();
        let ip = "203.0.113.9".to_string();
        for (i, at) in [100u64, 200, 5_000].into_iter().enumerate() {
            store
                .record_mint(ip.clone(), "identity", Some(format!("/k/a{i}")), at)
                .await
                .unwrap();
        }
        // A different IP keeps its own budget.
        store
            .record_mint("198.51.100.4".into(), "account", None, 5_000)
            .await
            .unwrap();

        // Window starting at 1_000 excludes the two early mints; lifetime counts all three.
        let (recent, lifetime) = store.mint_counts(ip.clone(), 1_000).await.unwrap();
        assert_eq!((recent, lifetime), (1, 3));

        let (recent, lifetime) = store.mint_counts(ip.clone(), 0).await.unwrap();
        assert_eq!((recent, lifetime), (3, 3));

        assert_eq!(
            store.oldest_mint_in_window(ip.clone(), 150).await.unwrap(),
            Some(200)
        );
        assert_eq!(
            store.oldest_mint_in_window(ip, 9_000).await.unwrap(),
            None,
            "no mints inside the window means nothing to wait for"
        );
    }

    #[tokio::test]
    async fn contacts_upsert_preserves_unset_axes() {
        let store = Store::open(":memory:").unwrap();
        let base = |admission, autonomy, now| ContactUpdate {
            owner: "/k/me".into(),
            peer: "/k/them".into(),
            alias: None,
            admission,
            autonomy,
            allowed_verbs: None,
            now,
        };

        let c = store.upsert_contact(base(None, None, 10)).await.unwrap();
        assert_eq!(
            (c.admission.as_str(), c.autonomy.as_str()),
            ("allow", "review")
        );
        assert_eq!(c.created_at, 10);

        // Change one axis; the other and the creation time survive.
        let c = store
            .upsert_contact(base(None, Some("auto".into()), 20))
            .await
            .unwrap();
        assert_eq!(
            (c.admission.as_str(), c.autonomy.as_str()),
            ("allow", "auto")
        );
        assert_eq!((c.created_at, c.updated_at), (10, 20));

        let c = store
            .upsert_contact(ContactUpdate {
                alias: Some("agent-B".into()),
                ..base(Some("block".into()), None, 30)
            })
            .await
            .unwrap();
        assert_eq!(c.autonomy, "auto", "unset axis must not be reset");
        assert_eq!(c.alias.as_deref(), Some("agent-B"));

        assert_eq!(store.list_contacts("/k/me".into()).await.unwrap().len(), 1);
        assert!(store
            .list_contacts("/k/other".into())
            .await
            .unwrap()
            .is_empty());

        assert!(store
            .delete_contact("/k/me".into(), "/k/them".into())
            .await
            .unwrap());
        assert!(
            !store
                .delete_contact("/k/me".into(), "/k/them".into())
                .await
                .unwrap(),
            "deleting twice reports nothing removed"
        );
        assert!(store
            .contact("/k/me".into(), "/k/them".into())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn inbox_policy_defaults_open_and_manual() {
        let store = Store::open(":memory:").unwrap();
        let default = store.inbox_policy("/k/me".into()).await.unwrap();
        assert_eq!(
            default,
            InboxPolicy {
                accept_all: true,
                auto_accept_known: false
            },
            "an inbox that never set a policy stays open to strangers and manual"
        );

        let set = store
            .set_inbox_policy("/k/me".into(), Some(false), None, 5)
            .await
            .unwrap();
        assert!(!set.accept_all);
        assert!(!set.auto_accept_known, "unset field keeps its value");

        let set = store
            .set_inbox_policy("/k/me".into(), None, Some(true), 6)
            .await
            .unwrap();
        assert_eq!(
            (set.accept_all, set.auto_accept_known),
            (false, true),
            "each field is independently persistent"
        );
        assert_eq!(store.inbox_policy("/k/me".into()).await.unwrap(), set);
    }

    #[tokio::test]
    async fn mint_counts_are_zero_for_an_unseen_ip() {
        let store = Store::open(":memory:").unwrap();
        assert_eq!(
            store.mint_counts("192.0.2.1".into(), 0).await.unwrap(),
            (0, 0)
        );
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
        let key = NewKey {
            key_hash: [7; 32],
            id: "key1".into(),
            prefix: "pk_live_aaaabbbb".into(),
        };
        store.create_account("acct_1".into(), key, 0).await.unwrap();
        assert_eq!(
            store.account_for_key([7; 32]).await.unwrap().as_deref(),
            Some("acct_1")
        );
        assert_eq!(store.account_for_key([9; 32]).await.unwrap(), None);

        let mut a = sample("/k/mine");
        a.account_id = Some("acct_1".into());
        store.insert(a).await.unwrap();
        store.insert(sample("/k/anon")).await.unwrap(); // no account

        let owned = store.list_by_account("acct_1".into()).await.unwrap();
        assert_eq!(
            owned,
            vec![("/k/mine".to_string(), Some("repo:test".to_string()))]
        );
        assert!(store
            .get_in_account("acct_1".into(), "/k/mine".into())
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_in_account("acct_1".into(), "/k/anon".into())
            .await
            .unwrap()
            .is_none());

        assert_eq!(store.count_for_account("acct_1".into()).await.unwrap(), 1);
        assert_eq!(
            store.count_for_account("acct_none".into()).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn key_management_list_revoke() {
        let store = Store::open(":memory:").unwrap();
        let first = NewKey {
            key_hash: [1; 32],
            id: "k_first".into(),
            prefix: "pk_live_first000".into(),
        };
        store
            .create_account("acct_1".into(), first, 100)
            .await
            .unwrap();
        // a second key on the same account
        let second = NewKey {
            key_hash: [2; 32],
            id: "k_second".into(),
            prefix: "pk_live_secnd000".into(),
        };
        store
            .add_api_key("acct_1".into(), second, 200)
            .await
            .unwrap();

        let mut keys = store.list_keys("acct_1".into()).await.unwrap();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].0, "k_first");
        assert_eq!(keys[0].1, "pk_live_first000");
        assert_eq!(keys[1].0, "k_second");

        // both keys authenticate before revocation
        assert_eq!(
            store.account_for_key([1; 32]).await.unwrap().as_deref(),
            Some("acct_1")
        );
        assert_eq!(
            store.account_for_key([2; 32]).await.unwrap().as_deref(),
            Some("acct_1")
        );

        // revoke the second; it no longer authenticates, the first still does
        assert!(store
            .revoke_key("acct_1".into(), "k_second".into())
            .await
            .unwrap());
        assert!(!store
            .revoke_key("acct_1".into(), "k_second".into())
            .await
            .unwrap()); // idempotent
        assert_eq!(store.account_for_key([2; 32]).await.unwrap(), None);
        assert_eq!(
            store.account_for_key([1; 32]).await.unwrap().as_deref(),
            Some("acct_1")
        );

        // can't revoke another account's key
        assert!(!store
            .revoke_key("acct_other".into(), "k_first".into())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn delete_identity_scoped_and_removes_messages() {
        let store = Store::open(":memory:").unwrap();
        let key = NewKey {
            key_hash: [3; 32],
            id: "k".into(),
            prefix: "pk_live_x".into(),
        };
        store.create_account("acct_1".into(), key, 0).await.unwrap();

        let mut mine = sample("/k/mine");
        mine.account_id = Some("acct_1".into());
        store.insert(mine).await.unwrap();
        store.enqueue(msg("m1", "/k/mine", 1)).await.unwrap();
        assert_eq!(store.inbox_count("/k/mine".into()).await.unwrap(), 1);

        // wrong account can't delete it
        assert!(!store
            .delete_identity(Some("acct_other".into()), "/k/mine".into())
            .await
            .unwrap());
        assert!(store.get("/k/mine".into()).await.unwrap().is_some());

        // owner deletes it: identity and its messages go
        assert!(store
            .delete_identity(Some("acct_1".into()), "/k/mine".into())
            .await
            .unwrap());
        assert!(!store
            .delete_identity(Some("acct_1".into()), "/k/mine".into())
            .await
            .unwrap()); // idempotent
        assert!(store.get("/k/mine".into()).await.unwrap().is_none());
        assert_eq!(store.inbox_count("/k/mine".into()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn inbox_count_tracks_messages() {
        let store = Store::open(":memory:").unwrap();
        assert_eq!(store.inbox_count("/k/box".into()).await.unwrap(), 0);
        store.enqueue(msg("a", "/k/box", 1)).await.unwrap();
        store.enqueue(msg("b", "/k/box", 2)).await.unwrap();
        store.enqueue(msg("c", "/k/other", 3)).await.unwrap();
        assert_eq!(store.inbox_count("/k/box".into()).await.unwrap(), 2);
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

#[cfg(test)]
mod handle_binding_tests {
    use super::*;

    fn owned(addr: &str, account: &str, handle: Option<&str>) -> StoredIdentity {
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
            account_id: Some(account.to_string()),
            handle: handle.map(str::to_string),
        }
    }

    /// The whole point of the retrofit: the address does not move.
    #[tokio::test]
    async fn binding_keeps_the_address() {
        let store = Store::open(":memory:").unwrap();
        store.insert(owned("/k/aaa", "acct_1", None)).await.unwrap();

        let outcome = store
            .bind_handle(
                "/k/aaa".into(),
                "/bekir/agent1".into(),
                "acct_1".into(),
                "/bekir".into(),
                100,
            )
            .await
            .unwrap();

        assert_eq!(outcome, BindOutcome::Bound);
        let found = store.get_by_handle("/bekir/agent1".into()).await.unwrap();
        assert_eq!(found.unwrap().address, "/k/aaa");
    }

    #[tokio::test]
    async fn another_accounts_mailbox_is_refused() {
        let store = Store::open(":memory:").unwrap();
        store.insert(owned("/k/aaa", "acct_1", None)).await.unwrap();
        let outcome = store
            .bind_handle(
                "/k/aaa".into(),
                "/bekir/agent1".into(),
                "acct_2".into(),
                "/bekir".into(),
                100,
            )
            .await
            .unwrap();
        assert_eq!(outcome, BindOutcome::NotYours);
    }

    /// Renaming would strand every contact entry trusting the old name, so it is refused outright.
    #[tokio::test]
    async fn a_named_mailbox_will_not_be_renamed() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(owned("/k/aaa", "acct_1", Some("/bekir/first")))
            .await
            .unwrap();
        let outcome = store
            .bind_handle(
                "/k/aaa".into(),
                "/bekir/second".into(),
                "acct_1".into(),
                "/bekir".into(),
                100,
            )
            .await
            .unwrap();
        assert_eq!(outcome, BindOutcome::AlreadyNamed("/bekir/first".into()));
    }

    #[tokio::test]
    async fn a_taken_handle_is_refused() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(owned("/k/aaa", "acct_1", Some("/bekir/agent1")))
            .await
            .unwrap();
        store.insert(owned("/k/bbb", "acct_1", None)).await.unwrap();
        let outcome = store
            .bind_handle(
                "/k/bbb".into(),
                "/bekir/agent1".into(),
                "acct_1".into(),
                "/bekir".into(),
                100,
            )
            .await
            .unwrap();
        assert_eq!(outcome, BindOutcome::Taken);
    }

    #[tokio::test]
    async fn the_ceiling_is_enforced_on_both_paths() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(owned("/k/held", "acct_1", Some("/bekir/held")))
            .await
            .unwrap();

        // Minting into a full namespace.
        let outcome = store
            .insert_under_namespace(
                owned("/k/new", "acct_1", Some("/bekir/new")),
                "/bekir".into(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(outcome, QuotaOutcome::Full);

        // …and naming an existing mailbox into one.
        store.insert(owned("/k/bbb", "acct_1", None)).await.unwrap();
        let outcome = store
            .bind_handle(
                "/k/bbb".into(),
                "/bekir/agent1".into(),
                "acct_1".into(),
                "/bekir".into(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(outcome, BindOutcome::Full);
    }

    /// The race the quota moved inside the transaction to close.
    ///
    /// Both mints read the same count if the check happens before the write. Exactly one may win.
    #[tokio::test]
    async fn concurrent_mints_at_the_boundary_yield_one_winner() {
        let store = Store::open(":memory:").unwrap();
        // One slot left out of two.
        store
            .insert(owned("/k/held", "acct_1", Some("/bekir/held")))
            .await
            .unwrap();

        let a = store.insert_under_namespace(
            owned("/k/aaa", "acct_1", Some("/bekir/a")),
            "/bekir".into(),
            2,
        );
        let b = store.insert_under_namespace(
            owned("/k/bbb", "acct_1", Some("/bekir/b")),
            "/bekir".into(),
            2,
        );
        let (a, b) = tokio::join!(a, b);

        let inserted = [a.unwrap(), b.unwrap()]
            .into_iter()
            .filter(|o| *o == QuotaOutcome::Inserted)
            .count();
        assert_eq!(inserted, 1, "exactly one mint may take the last slot");
        assert_eq!(store.count_for_namespace("/bekir".into()).await.unwrap(), 2);
    }
}
