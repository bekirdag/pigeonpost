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

-- One row per copy of a message, not per message: a delivered copy sealed to the recipient, and —
-- when the sender is hosted here — a sent copy sealed to the sender. `owner` is whose mailbox the
-- row lives in; `sender`/`recipient` describe the message itself.
CREATE TABLE IF NOT EXISTS messages (
    id         TEXT PRIMARY KEY,
    recipient  TEXT NOT NULL,
    sender     TEXT NOT NULL,
    wrap_blob  BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    read       INTEGER NOT NULL DEFAULT 0,
    owner      TEXT,
    direction  TEXT NOT NULL DEFAULT 'in'
);
CREATE INDEX IF NOT EXISTS messages_by_recipient ON messages(recipient);
-- The index on `owner` lives in MIGRATIONS, not here: on a database created before that column
-- existed this batch runs first, and indexing a column the ALTER has not added yet fails the whole
-- open.

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

-- Threads the owner has put out of sight.
--
-- Archiving is about attention, not secrecy: the mail is untouched, still delivered, still
-- readable, and a peer who writes again is still heard. Only the reader's own view of it changes.
-- So this is a list of peers rather than of messages — a conversation is what someone is finished
-- with, and a per-message flag would need re-applying every time the same peer wrote again.
--
-- Server-side rather than in the browser because it has to hold across the phone, the laptop, and
-- whatever reads it next. A view that differs per device is not a filed conversation, it is a
-- conversation someone has to file repeatedly.
CREATE TABLE IF NOT EXISTS archived_threads (
    owner       TEXT NOT NULL,
    peer        TEXT NOT NULL,
    archived_at INTEGER NOT NULL,
    PRIMARY KEY (owner, peer)
);

-- Conversations, cut into subjects.
--
-- A thread belongs to the *pair*, not to one side, so its id travels with the message and both
-- mailboxes store the same one. If each side grouped locally instead, the sender's \"deploy
-- question\" and the recipient's would be unrelated rows and a reply would land in neither.
--
-- `title` is set by whoever opened the thread and copied to both sides once. It is not kept in
-- sync afterwards: a shared mutable title is a field one peer can rewrite inside the other's
-- inbox, and renaming your own view of a conversation is a local act everywhere else.
--
-- Every message is in a thread, including every message that predates this table — a per-pair
-- default thread is created for them, so nothing downstream has to handle \"no thread\". A peer
-- with only that default thread looks exactly as it did before threads existed, which is the
-- point: the common case does not pay for the feature.
CREATE TABLE IF NOT EXISTS threads (
    -- Not a primary key on its own: the same thread exists once in each participant's mailbox, so
    -- `id` repeats exactly twice and it is (id, owner) that identifies a row. Keying on `id` alone
    -- silently drops the second side, which looks like the recipient never being told about the
    -- thread at all.
    id          TEXT NOT NULL,
    owner       TEXT NOT NULL,
    peer        TEXT NOT NULL,
    title       TEXT,
    -- The one a message joins when nobody named a thread. Exactly one per pair, enforced below.
    is_default  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    last_at     INTEGER NOT NULL,
    -- Per-thread filing, separate from archiving a whole peer. Both are useful: one finishes a
    -- subject, the other finishes a correspondent.
    archived_at INTEGER,
    PRIMARY KEY (id, owner)
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

-- Provider identities an account has *proved* it controls, which is how `/github/<login>` is
-- authorised.
--
-- A provider namespace cannot work like a purchased one: `/bekir` has a single owner, but nobody
-- owns all of `/github`. Ownership there is per name and has to be earned by proof, so this table
-- is the record of that proof — one row per verified login, and the only thing that lets an account
-- mint under it.
--
-- `provider_user_id` is the account's immutable id at the provider, kept because logins are
-- renameable and reusable: if someone gives up a login and another person takes it, the id is what
-- distinguishes them, and re-verification is refused rather than silently handing over the mailbox.
CREATE TABLE IF NOT EXISTS provider_identities (
    provider         TEXT NOT NULL,
    login            TEXT NOT NULL,
    provider_user_id TEXT NOT NULL,
    account_id       TEXT NOT NULL,
    verified_at      INTEGER NOT NULL,
    PRIMARY KEY (provider, login)
);
CREATE INDEX IF NOT EXISTS provider_identities_by_account
    ON provider_identities(provider, account_id);

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

-- Devices that want waking when a mailbox receives mail. One row per (mailbox, token): a phone
-- registers for the mailbox it is reading as, and a mailbox reaches every device watching it.
--
-- `environment` is not decoration. A token minted by a TestFlight or App Store build belongs to
-- production APNs and one from a build run out of Xcode belongs to sandbox; each is meaningless to
-- the other, and a token sent to the wrong one fails in a way that looks like a dead device.
CREATE TABLE IF NOT EXISTS devices (
    token       TEXT PRIMARY KEY,
    mailbox     TEXT NOT NULL,
    account     TEXT,
    platform    TEXT NOT NULL DEFAULT 'apns',
    environment TEXT NOT NULL DEFAULT 'production',
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS devices_by_mailbox ON devices(mailbox);

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
    // A conversation has two sides. Until now the postbox stored only the delivered copy, sealed to
    // the recipient, so a sender could never read back what they sent — their own outbox existed
    // nowhere. `owner` is the mailbox a row lives in, which is what every lookup actually means;
    // `sender` and `recipient` keep describing the message rather than doubling as the location.
    //
    // A sent copy is a second row: owner = sender, direction = 'out', sealed to the sender's own
    // key. Every pre-existing row is a delivered one, so it backfills to owner = recipient.
    "ALTER TABLE messages ADD COLUMN owner TEXT",
    "ALTER TABLE messages ADD COLUMN direction TEXT NOT NULL DEFAULT 'in'",
    "UPDATE messages SET owner = recipient WHERE owner IS NULL",
    "CREATE INDEX IF NOT EXISTS messages_by_owner ON messages(owner)",
    // Threads. The column is nullable only for the instant between adding it and the backfill
    // below; every read treats a NULL thread as a bug rather than a state to handle.
    "ALTER TABLE messages ADD COLUMN thread_id TEXT",
    "CREATE INDEX IF NOT EXISTS messages_by_thread ON messages(thread_id)",
    "CREATE INDEX IF NOT EXISTS threads_by_pair ON threads(owner, peer, last_at DESC)",
    // One default thread per pair, and no more. A partial unique index says exactly that, and
    // lets the backfill and every later send use INSERT OR IGNORE instead of a read-then-write
    // race between two messages arriving at once.
    "CREATE UNIQUE INDEX IF NOT EXISTS threads_default_per_pair ON threads(owner, peer) WHERE is_default = 1",
    // Give every conversation that predates threads its default one. The peer of a row is the
    // other end of it, which is the recipient on a sent copy and the sender on a delivered one.
    //
    // Done in two steps because both directions of a pair must end up with the *same* id, and a
    // random id per group would hand each side its own. The helper table draws one id per unordered
    // pair first; the insert then joins on it.
    "CREATE TABLE IF NOT EXISTS thread_backfill_ids (
         lo TEXT NOT NULL, hi TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY (lo, hi))",
    "INSERT OR IGNORE INTO thread_backfill_ids (lo, hi, id)
       SELECT DISTINCT
              CASE WHEN owner < peer THEN owner ELSE peer END,
              CASE WHEN owner < peer THEN peer ELSE owner END,
              lower(hex(randomblob(16)))
         FROM (SELECT owner,
                      CASE WHEN direction = 'out' THEN recipient ELSE sender END AS peer
                 FROM messages
                WHERE owner IS NOT NULL)",
    "INSERT OR IGNORE INTO threads (id, owner, peer, title, is_default, created_at, last_at)
       SELECT ids.id, p.owner, p.peer, NULL, 1, p.first_at, p.last_at
         FROM (SELECT owner,
                      CASE WHEN direction = 'out' THEN recipient ELSE sender END AS peer,
                      MIN(created_at) AS first_at,
                      MAX(created_at) AS last_at
                 FROM messages
                WHERE owner IS NOT NULL
                GROUP BY owner, CASE WHEN direction = 'out' THEN recipient ELSE sender END) p
         JOIN thread_backfill_ids ids
           ON ids.lo = CASE WHEN p.owner < p.peer THEN p.owner ELSE p.peer END
          AND ids.hi = CASE WHEN p.owner < p.peer THEN p.peer ELSE p.owner END",
    "UPDATE messages SET thread_id = (
         SELECT t.id FROM threads t
          WHERE t.owner = messages.owner
            AND t.peer = CASE WHEN messages.direction = 'out' THEN messages.recipient ELSE messages.sender END
            AND t.is_default = 1)
       WHERE thread_id IS NULL",
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

/// Why a proved provider identity was still not recorded.
///
/// Both cases mean "this login is spoken for", but they are different facts and the caller answers
/// them differently — one is a name that changed hands, the other is the same person signed in to a
/// second Pigeonpost account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderClaimRefusal {
    /// The login now belongs to a different provider account than the one that first proved it.
    DifferentPerson,
    /// Already proved, by another Pigeonpost account.
    HeldByAnotherAccount,
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
    /// Whose mailbox this copy lives in. For a delivered copy that is the recipient; for a sent
    /// copy, the sender.
    pub owner: String,
    /// `true` when this is the sender's own copy, sealed to the sender's key.
    pub outgoing: bool,
    /// Which thread this copy belongs to, in the owner's mailbox. Both sides of a conversation
    /// carry the same id, because a thread is a property of the pair rather than of one inbox.
    pub thread_id: String,
}

fn read_thread(row: &rusqlite::Row<'_>) -> Result<Thread, StoreError> {
    Ok(Thread {
        id: row.get(0)?,
        owner: row.get(1)?,
        peer: row.get(2)?,
        title: row.get(3)?,
        is_default: row.get::<_, i64>(4)? != 0,
        created_at: row.get::<_, i64>(5)? as u64,
        last_at: row.get::<_, i64>(6)? as u64,
        archived_at: row.get::<_, Option<i64>>(7)?.map(|a| a as u64),
    })
}

/// One subject inside a conversation with a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    pub owner: String,
    pub peer: String,
    /// `None` for the default thread, which has no name because nobody chose to open it.
    pub title: Option<String>,
    pub is_default: bool,
    pub created_at: u64,
    pub last_at: u64,
    pub archived_at: Option<u64>,
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

/// A device waiting to be told that mail arrived.
#[derive(Debug, Clone)]
pub struct Device {
    pub token: String,
    pub mailbox: String,
    pub platform: String,
    pub environment: String,
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
    /// The mailbox exists but belongs to another account, or the proof of control did not match.
    NotYours,
    /// The mailbox belongs to no account, so control of it must be proven with its capability
    /// token before an account may adopt and name it.
    ProofRequired,
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

    /// How many copies a mailbox holds (inbox quota check). Counts sent copies too: they occupy the
    /// same disk, and the quota exists to bound disk.
    /// Ensure this mailbox's default thread with a peer exists under `id`.
    ///
    /// Separate from [`Store::create_thread`] only in setting `is_default`. The id is supplied so
    /// both halves of a pair can share one: two independently generated defaults would mean a
    /// client replying by the id it can see would open a *second* thread in the other mailbox.
    pub async fn ensure_default_thread(
        &self,
        id: String,
        owner: String,
        peer: String,
        now: u64,
    ) -> Result<String, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<String, StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT OR IGNORE INTO threads (id, owner, peer, title, is_default, created_at, last_at)
                 VALUES (?1, ?2, ?3, NULL, 1, ?4, ?4)",
                params![id, owner, peer, now as i64],
            )?;
            // Re-read rather than trusting the insert: the partial unique index means a concurrent
            // send may have won, and its id is the one that counts.
            Ok(c.query_row(
                "SELECT id FROM threads WHERE owner = ?1 AND peer = ?2 AND is_default = 1",
                params![owner, peer],
                |r| r.get(0),
            )?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// This mailbox's default thread with a peer, if they have one yet.
    pub async fn default_thread_id(
        &self,
        owner: String,
        peer: String,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut q = c.prepare(
                "SELECT id FROM threads WHERE owner = ?1 AND peer = ?2 AND is_default = 1",
            )?;
            let mut rows = q.query(params![owner, peer])?;
            match rows.next()? {
                Some(row) => Ok(Some(row.get(0)?)),
                None => Ok(None),
            }
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Open a named thread with a peer, in one mailbox.
    ///
    /// The id is supplied rather than generated, because both sides of a conversation must end up
    /// with the same one and only the sender's side knows it first.
    pub async fn create_thread(
        &self,
        id: String,
        owner: String,
        peer: String,
        title: Option<String>,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT OR IGNORE INTO threads (id, owner, peer, title, is_default, created_at, last_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
                params![id, owner, peer, title, now as i64],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// One thread, if it is in this mailbox. Scoped to `owner` so an id from elsewhere reads as
    /// absent rather than as somebody else's conversation.
    pub async fn thread(&self, owner: String, id: String) -> Result<Option<Thread>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Thread>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut q = c.prepare(
                "SELECT id, owner, peer, title, is_default, created_at, last_at, archived_at
                   FROM threads WHERE owner = ?1 AND id = ?2",
            )?;
            let mut rows = q.query(params![owner, id])?;
            match rows.next()? {
                Some(row) => Ok(Some(read_thread(row)?)),
                None => Ok(None),
            }
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// This mailbox's threads, most recently active first. `peer` narrows it to one correspondent.
    pub async fn threads(
        &self,
        owner: String,
        peer: Option<String>,
    ) -> Result<Vec<Thread>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Thread>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut q = c.prepare(
                "SELECT id, owner, peer, title, is_default, created_at, last_at, archived_at
                   FROM threads
                  WHERE owner = ?1 AND (?2 IS NULL OR peer = ?2)
                  ORDER BY last_at DESC",
            )?;
            let mut rows = q.query(params![owner, peer])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(read_thread(row)?);
            }
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Rename or file one thread. Local to this mailbox in both cases: a title the peer could
    /// rewrite is a title that is not yours, and filing is about one reader's attention.
    pub async fn update_thread(
        &self,
        owner: String,
        id: String,
        title: Option<Option<String>>,
        archived_at: Option<Option<u64>>,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut changed = 0;
            if let Some(title) = title {
                changed += c.execute(
                    "UPDATE threads SET title = ?3 WHERE owner = ?1 AND id = ?2",
                    params![owner, id, title],
                )?;
            }
            if let Some(archived_at) = archived_at {
                changed += c.execute(
                    "UPDATE threads SET archived_at = ?3 WHERE owner = ?1 AND id = ?2",
                    params![owner, id, archived_at.map(|a| a as i64)],
                )?;
            }
            Ok(changed > 0)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn inbox_count(&self, owner: String) -> Result<usize, StoreError> {
        self.count_where(
            "SELECT COUNT(*) FROM messages WHERE COALESCE(owner, recipient) = ?1",
            owner,
        )
        .await
    }

    /// Messages this inbox has not acked. This — not the total — is what a long poll waits on:
    /// an inbox holding only already-read mail is quiet, and waiting on the total would make a
    /// caller with one un-acked message spin instead of wait.
    /// Received and un-acked only. A sent copy must never wake a long poll: the caller is the one
    /// who wrote it, and waking them with their own message would turn every send into a spurious
    /// "you have mail".
    pub async fn unread_count(&self, owner: String) -> Result<usize, StoreError> {
        self.count_where(
            "SELECT COUNT(*) FROM messages
              WHERE COALESCE(owner, recipient) = ?1 AND read = 0 AND direction = 'in'",
            owner,
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
        proof: Option<[u8; 32]>,
    ) -> Result<BindOutcome, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<BindOutcome, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

            let existing: Option<(Option<String>, Option<String>, Vec<u8>)> = tx
                .query_row(
                    "SELECT account_id, handle, cap_hash FROM identities WHERE address = ?1",
                    params![address],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((owner, current, cap_hash)) = existing else {
                return Ok(BindOutcome::NoSuchMailbox);
            };

            // Two ways to be allowed to name a mailbox, and the second is the one that matters.
            //
            // An account naming a mailbox it already owns is the easy case. But the mailbox this
            // feature exists for is the one an agent minted anonymously, before anyone bought a
            // namespace — it has no account at all. Refusing those would strand exactly the
            // agents the retrofit is for.
            //
            // Letting an account claim *any* unowned mailbox would be far worse: anyone could
            // seize anyone else's anonymous inbox by guessing an address. So an unowned mailbox
            // is adopted only on proof of *control* — the capability token, which is the sole
            // credential that mailbox has. Compared as a hash, in constant time.
            let owned_by_caller = owner.as_deref() == Some(account.as_str());
            if !owned_by_caller {
                if owner.is_some() {
                    return Ok(BindOutcome::NotYours);
                }
                let Some(proof) = proof else {
                    return Ok(BindOutcome::ProofRequired);
                };
                let matches = cap_hash.len() == 32
                    && cap_hash
                        .iter()
                        .zip(proof.iter())
                        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                        == 0;
                if !matches {
                    return Ok(BindOutcome::NotYours);
                }
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
            // Adopt it into the account at the same time: from here it spends a slot in that
            // account's namespace and must list under it, so ownership and name have to land
            // together or a crash between them would leave a named mailbox nobody owns.
            match tx.execute(
                "UPDATE identities SET handle = ?1, account_id = ?2 WHERE address = ?3",
                params![handle, account, address],
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

    /// The peers this mailbox has archived.
    pub async fn archived_threads(&self, owner: String) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT peer FROM archived_threads WHERE owner = ?1 ORDER BY archived_at DESC",
            )?;
            let rows = stmt.query_map(params![owner], |r| r.get(0))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// File a thread away, or bring it back. Idempotent in both directions: archiving what is
    /// already archived is what a second tap on the same button means.
    pub async fn set_thread_archived(
        &self,
        owner: String,
        peer: String,
        archived: bool,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            if archived {
                c.execute(
                    "INSERT INTO archived_threads (owner, peer, archived_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(owner, peer) DO UPDATE SET archived_at = excluded.archived_at",
                    params![owner, peer, now as i64],
                )?;
            } else {
                c.execute(
                    "DELETE FROM archived_threads WHERE owner = ?1 AND peer = ?2",
                    params![owner, peer],
                )?;
            }
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record that `account_id` proved control of `login` at `provider`.
    ///
    /// Re-verifying the same login from the same provider account is idempotent — people re-run
    /// setup, and a second proof of the same fact should not fail. A login whose provider user id
    /// has changed is a *different* person who acquired a released name: that is refused here
    /// rather than transferring the handle, because mail already addressed to it would follow.
    pub async fn record_provider_identity(
        &self,
        provider: String,
        login: String,
        provider_user_id: String,
        account_id: String,
        now: u64,
    ) -> Result<Result<(), ProviderClaimRefusal>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> Result<Result<(), ProviderClaimRefusal>, StoreError> {
                let c = conn.lock().expect("store lock");
                let existing: Option<(String, String)> = c
                    .query_row(
                        "SELECT provider_user_id, account_id FROM provider_identities
                          WHERE provider = ?1 AND login = ?2",
                        params![provider, login],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((seen_user_id, owner)) = existing {
                    if seen_user_id != provider_user_id {
                        return Ok(Err(ProviderClaimRefusal::DifferentPerson));
                    }
                    if owner != account_id {
                        return Ok(Err(ProviderClaimRefusal::HeldByAnotherAccount));
                    }
                    return Ok(Ok(()));
                }
                c.execute(
                    "INSERT INTO provider_identities
                       (provider, login, provider_user_id, account_id, verified_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![provider, login, provider_user_id, account_id, now as i64],
                )?;
                Ok(Ok(()))
            },
        )
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The account that proved control of `login` at `provider`, if any.
    pub async fn provider_identity_owner(
        &self,
        provider: String,
        login: String,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            c.query_row(
                "SELECT account_id FROM provider_identities WHERE provider = ?1 AND login = ?2",
                params![provider, login],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The mailbox that answers for a whole namespace — who receives when someone writes to
    /// `/bekir` rather than to `/bekir/agent1`.
    ///
    /// `/<namespace>/main` by convention, and the namespace's first mailbox when there is no
    /// `main`. The fallback is what makes writing to a namespace work the day it is bought instead
    /// of after someone remembers to create a mailbox by the right name; `created_at` orders it so
    /// the answer is stable rather than whichever row the planner returned first. Minting a `main`
    /// later moves the destination deliberately, which is the point of the convention.
    pub async fn namespace_inbox(
        &self,
        namespace: String,
    ) -> Result<Option<StoredIdentity>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<StoredIdentity>, StoreError> {
            let c = conn.lock().expect("store lock");
            let main = format!("/{namespace}/main");
            if let Some(row) = c
                .query_row(
                    &format!("SELECT {ID_COLS} FROM identities WHERE handle = ?1"),
                    params![main],
                    map_id_row,
                )
                .optional()?
            {
                return id_from_row(row).map(Some);
            }
            // `handle GLOB` rather than LIKE: the namespace can contain `_`, which LIKE treats as a
            // wildcard, and `/bekir_ops/x` must not answer for `/bekir`.
            let pattern = format!("/{namespace}/*");
            let row = c
                .query_row(
                    &format!(
                        "SELECT {ID_COLS} FROM identities WHERE handle GLOB ?1 \
                         ORDER BY created_at ASC, address ASC LIMIT 1"
                    ),
                    params![pattern],
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
                // Received mail only: you can report what was sent to you, never your own copy of
                // what you sent.
                "SELECT sender FROM messages
                  WHERE id = ?1 AND COALESCE(owner, recipient) = ?2 AND direction = 'in'",
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
                // Delivered copies only. Counting the sender's own copy as well would halve the
                // stranger allowance the moment sent copies started being stored.
                "SELECT COUNT(*) FROM messages
                  WHERE sender = ?1 AND recipient = ?2 AND created_at >= ?3
                    AND direction = 'in'",
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

    /// Register a device for a mailbox, or move an existing token to this mailbox.
    ///
    /// Keyed on the token rather than on (mailbox, token): iOS hands the same token to the app
    /// whichever mailbox it is reading, so two rows would mean two notifications for one message.
    pub async fn upsert_device(
        &self,
        token: String,
        mailbox: String,
        account: Option<String>,
        platform: String,
        environment: String,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let conn = conn.lock().expect("store lock");
            conn.execute(
                "INSERT INTO devices (token, mailbox, account, platform, environment, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(token) DO UPDATE SET
                     mailbox = excluded.mailbox,
                     account = excluded.account,
                     platform = excluded.platform,
                     environment = excluded.environment,
                     updated_at = excluded.updated_at",
                params![token, mailbox, account, platform, environment, now],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Every device watching one mailbox.
    pub async fn devices_for(&self, mailbox: String) -> Result<Vec<Device>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Device>, StoreError> {
            let conn = conn.lock().expect("store lock");
            let mut stmt = conn.prepare(
                "SELECT token, mailbox, platform, environment FROM devices WHERE mailbox = ?1",
            )?;
            let rows = stmt.query_map(params![mailbox], |row| {
                Ok(Device {
                    token: row.get(0)?,
                    mailbox: row.get(1)?,
                    platform: row.get(2)?,
                    environment: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Forget a device — on sign-out, or when Apple says the token is dead.
    pub async fn delete_device(&self, token: String) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let conn = conn.lock().expect("store lock");
            Ok(conn.execute("DELETE FROM devices WHERE token = ?1", params![token])? > 0)
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
                    "DELETE FROM messages
                      WHERE recipient = ?1 OR sender = ?1 OR owner = ?1",
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
                "INSERT INTO messages
                     (id, recipient, sender, wrap_blob, created_at, read, owner, direction, thread_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    m.id,
                    m.recipient,
                    m.sender,
                    m.wrap_blob,
                    m.created_at as i64,
                    m.read as i64,
                    m.owner,
                    if m.outgoing { "out" } else { "in" },
                    m.thread_id,
                ],
            )?;
            // A thread's ordering is by its own last activity, not by when it was opened, or a
            // long-running conversation sinks below one somebody named and abandoned.
            c.execute(
                "UPDATE threads SET last_at = ?2 WHERE id = ?1 AND last_at < ?2",
                params![m.thread_id, m.created_at as i64],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Every copy in `owner`'s mailbox, received and sent, oldest first — the whole conversation
    /// rather than half of it.
    ///
    /// `owner` is coalesced to `recipient` so a row written before that column existed still reads
    /// correctly even if the backfill has not run.
    /// Incoming messages for any of `owners` newer than `after`, with the cursor to resume from.
    ///
    /// The cursor is SQLite's `rowid`: monotonic, assigned at insert, and never reused while rows
    /// are only appended. That makes it exactly the resumption token an event stream needs — a
    /// client that reconnects with the last id it saw gets everything since and nothing twice,
    /// without the server holding per-client state.
    ///
    /// Sent copies are excluded: a stream exists to say "someone wrote to you", and echoing a
    /// mailbox's own outgoing mail back at it would wake an agent to read its own words.
    pub async fn incoming_after(
        &self,
        owners: Vec<String>,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, Message)>, StoreError> {
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<(i64, Message)>, StoreError> {
            let c = conn.lock().expect("store lock");
            let placeholders = owners.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT rowid, id, recipient, sender, wrap_blob, created_at, read,
                        COALESCE(owner, recipient), direction, thread_id
                 FROM messages
                 WHERE COALESCE(owner, recipient) IN ({placeholders})
                   AND rowid > ?
                   AND direction != 'out'
                 ORDER BY rowid ASC LIMIT ?"
            );
            let mut stmt = c.prepare(&sql)?;
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = owners
                .iter()
                .map(|o| Box::new(o.clone()) as Box<dyn rusqlite::ToSql>)
                .collect();
            params.push(Box::new(after));
            params.push(Box::new(limit as i64));
            let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    Message {
                        id: row.get(1)?,
                        recipient: row.get(2)?,
                        sender: row.get(3)?,
                        wrap_blob: row.get(4)?,
                        created_at: row.get::<_, i64>(5)? as u64,
                        read: row.get::<_, i64>(6)? != 0,
                        owner: row.get(7)?,
                        outgoing: row.get::<_, String>(8)? == "out",
                        thread_id: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    },
                ))
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

    /// The highest message rowid, so a stream can start from "only what happens next" rather than
    /// replaying a mailbox's whole history to a daemon that just connected.
    pub async fn latest_message_cursor(&self) -> Result<i64, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<i64, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(
                c.query_row("SELECT COALESCE(MAX(rowid), 0) FROM messages", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn list_for(&self, owner: String) -> Result<Vec<Message>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT id, recipient, sender, wrap_blob, created_at, read,
                        COALESCE(owner, recipient), direction, COALESCE(thread_id, '')
                 FROM messages WHERE COALESCE(owner, recipient) = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![owner], |row| {
                Ok(Message {
                    id: row.get(0)?,
                    recipient: row.get(1)?,
                    sender: row.get(2)?,
                    wrap_blob: row.get(3)?,
                    created_at: row.get::<_, i64>(4)? as u64,
                    read: row.get::<_, i64>(5)? != 0,
                    owner: row.get(6)?,
                    outgoing: row.get::<_, String>(7)? == "out",
                    thread_id: row.get(8)?,
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
                "UPDATE messages SET read = 1
                  WHERE id = ?1 AND COALESCE(owner, recipient) = ?2",
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
                    OR sender    IN (SELECT address FROM identities WHERE created_at < ?1)
                    OR owner     IN (SELECT address FROM identities WHERE created_at < ?1)",
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

    #[tokio::test]
    async fn mail_written_before_the_sent_copy_existed_still_reads() {
        // The live database predates `owner` and `direction`. Its rows are all delivered mail, and
        // they must keep reading correctly — this is the migration that runs against real inboxes.
        let dir = std::env::temp_dir().join(format!("pp-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.db");
        let path_str = path.to_str().unwrap().to_string();

        // Exactly the pre-change table.
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE messages (
                     id TEXT PRIMARY KEY, recipient TEXT NOT NULL, sender TEXT NOT NULL,
                     wrap_blob BLOB NOT NULL, created_at INTEGER NOT NULL,
                     read INTEGER NOT NULL DEFAULT 0
                 );",
            )
            .unwrap();
            c.execute(
                "INSERT INTO messages (id, recipient, sender, wrap_blob, created_at, read)
                 VALUES ('old1', '/k/bob', '/k/alice', X'010203', 1000, 0)",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&path_str).unwrap();

        let msgs = store.list_for("/k/bob".into()).await.unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "pre-existing mail must survive the migration"
        );
        assert_eq!(msgs[0].id, "old1");
        assert_eq!(msgs[0].owner, "/k/bob", "backfilled from the recipient");
        assert!(!msgs[0].outgoing, "everything written before was delivered");

        // And the counts it feeds still see it.
        assert_eq!(store.inbox_count("/k/bob".into()).await.unwrap(), 1);
        assert_eq!(store.unread_count("/k/bob".into()).await.unwrap(), 1);
        assert!(store
            .mark_read("old1".into(), "/k/bob".into())
            .await
            .unwrap());
        assert_eq!(store.unread_count("/k/bob".into()).await.unwrap(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

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

    fn named(addr: &str, handle: &str, created_at: u64) -> StoredIdentity {
        StoredIdentity {
            handle: Some(handle.to_string()),
            created_at,
            ..sample(addr)
        }
    }

    /// `/<namespace>/main` is the convention, so it wins outright once it exists.
    #[tokio::test]
    async fn a_namespace_prefers_its_main_mailbox() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(named("/k/first", "/bekir/agent1", 10))
            .await
            .unwrap();
        store
            .insert(named("/k/main", "/bekir/main", 99))
            .await
            .unwrap();

        let found = store
            .namespace_inbox("bekir".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            found.address, "/k/main",
            "main answers for the namespace even though it was created last"
        );
    }

    /// And without a `main`, writing to a namespace still has to reach somebody — otherwise the
    /// address only works after a setup step nobody was told about.
    #[tokio::test]
    async fn a_namespace_without_a_main_falls_back_to_its_first_mailbox() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(named("/k/second", "/bekir/agent2", 20))
            .await
            .unwrap();
        store
            .insert(named("/k/first", "/bekir/agent1", 10))
            .await
            .unwrap();

        let found = store
            .namespace_inbox("bekir".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.address, "/k/first", "oldest, so the answer is stable");
    }

    /// `_` is a LIKE wildcard and a legal namespace character, so the neighbouring namespace
    /// `/bekir_ops` must never answer for `/bekir`.
    #[tokio::test]
    async fn a_namespace_never_answers_for_a_similarly_named_one() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(named("/k/ops", "/bekir_ops/agent1", 10))
            .await
            .unwrap();
        assert!(
            store
                .namespace_inbox("bekir".into())
                .await
                .unwrap()
                .is_none(),
            "a namespace with no mailboxes has no inbox, however similar its neighbour"
        );
    }

    /// Archiving is one reader's view, not a property of the conversation: it must not follow the
    /// peer into anyone else's mailbox, and unarchiving must genuinely restore.
    #[tokio::test]
    async fn archiving_is_per_mailbox_and_reversible() {
        let store = Store::open(":memory:").unwrap();
        store
            .set_thread_archived("/k/me".into(), "/bekir/noisy".into(), true, 10)
            .await
            .unwrap();

        assert_eq!(
            store.archived_threads("/k/me".into()).await.unwrap(),
            vec!["/bekir/noisy".to_string()]
        );
        assert!(
            store
                .archived_threads("/k/someone-else".into())
                .await
                .unwrap()
                .is_empty(),
            "one reader filing a thread must not file it for another"
        );

        // Tapping archive twice is not an error, and it is not two rows.
        store
            .set_thread_archived("/k/me".into(), "/bekir/noisy".into(), true, 20)
            .await
            .unwrap();
        assert_eq!(
            store.archived_threads("/k/me".into()).await.unwrap().len(),
            1
        );

        store
            .set_thread_archived("/k/me".into(), "/bekir/noisy".into(), false, 30)
            .await
            .unwrap();
        assert!(store
            .archived_threads("/k/me".into())
            .await
            .unwrap()
            .is_empty());
    }

    /// A login that changed hands must not carry its handle to the new holder: mail already
    /// addressed to it would follow the name to a stranger.
    #[tokio::test]
    async fn a_reused_provider_login_is_refused_rather_than_transferred() {
        let store = Store::open(":memory:").unwrap();
        store
            .record_provider_identity(
                "github".into(),
                "ada".into(),
                "1".into(),
                "acct-a".into(),
                0,
            )
            .await
            .unwrap()
            .unwrap();

        // Same person, same account, run twice: setup gets re-run, and that must be fine.
        assert!(store
            .record_provider_identity(
                "github".into(),
                "ada".into(),
                "1".into(),
                "acct-a".into(),
                1
            )
            .await
            .unwrap()
            .is_ok());

        // Same login, different GitHub account: somebody took a released name.
        assert_eq!(
            store
                .record_provider_identity(
                    "github".into(),
                    "ada".into(),
                    "2".into(),
                    "acct-b".into(),
                    2
                )
                .await
                .unwrap(),
            Err(ProviderClaimRefusal::DifferentPerson)
        );

        // Same GitHub person, second Pigeonpost account: also refused, but for a different reason.
        assert_eq!(
            store
                .record_provider_identity(
                    "github".into(),
                    "ada".into(),
                    "1".into(),
                    "acct-b".into(),
                    3
                )
                .await
                .unwrap(),
            Err(ProviderClaimRefusal::HeldByAnotherAccount)
        );

        assert_eq!(
            store
                .provider_identity_owner("github".into(), "ada".into())
                .await
                .unwrap()
                .as_deref(),
            Some("acct-a")
        );
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
                owner: bob_addr.clone(),
                outgoing: false,
                thread_id: "t-test".into(),
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
            owner: recipient.into(),
            outgoing: false,
            thread_id: "t-test".into(),
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
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome, BindOutcome::Bound);
        let found = store.get_by_handle("/bekir/agent1".into()).await.unwrap();
        assert_eq!(found.unwrap().address, "/k/aaa");
    }

    /// The mailbox this feature exists for: minted anonymously, owned by nobody. It is adopted on
    /// proof of control, and refused without it.
    #[tokio::test]
    async fn an_unowned_mailbox_is_adopted_only_on_proof() {
        let store = Store::open(":memory:").unwrap();
        let mut id = owned("/k/orphan", "unused", None);
        id.account_id = None;
        id.cap_hash = [7; 32];
        store.insert(id).await.unwrap();

        let bind = |proof| {
            store.bind_handle(
                "/k/orphan".into(),
                "/bekir/adopted".into(),
                "acct_1".into(),
                "/bekir".into(),
                100,
                proof,
            )
        };

        assert_eq!(bind(None).await.unwrap(), BindOutcome::ProofRequired);
        assert_eq!(bind(Some([9; 32])).await.unwrap(), BindOutcome::NotYours);
        assert_eq!(bind(Some([7; 32])).await.unwrap(), BindOutcome::Bound);

        // Naming it also moves it into the account, since it now spends that namespace's slot.
        let found = store
            .get_by_handle("/bekir/adopted".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.address, "/k/orphan");
        assert_eq!(found.account_id.as_deref(), Some("acct_1"));
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
                None,
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
                None,
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
                None,
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
                None,
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
        // …and the loser left nothing behind: exactly one of the two names exists.
        let a = store.get_by_handle("/bekir/a".into()).await.unwrap();
        let b = store.get_by_handle("/bekir/b".into()).await.unwrap();
        assert_eq!(a.is_some() as u8 + b.is_some() as u8, 1);
    }
}

#[cfg(test)]
mod event_cursor_tests {
    use super::*;

    fn msg(id: &str, owner: &str, sender: &str, outgoing: bool) -> Message {
        Message {
            thread_id: "t-test".into(),
            id: id.into(),
            recipient: owner.into(),
            sender: sender.into(),
            wrap_blob: vec![1, 2, 3],
            created_at: 0,
            read: false,
            owner: owner.into(),
            outgoing,
        }
    }

    /// The cursor is the resumption contract: everything after it, nothing twice.
    #[tokio::test]
    async fn the_cursor_returns_each_message_exactly_once() {
        let store = Store::open(":memory:").unwrap();
        store
            .enqueue(msg("m1", "/k/bob", "/k/alice", false))
            .await
            .unwrap();
        store
            .enqueue(msg("m2", "/k/bob", "/k/alice", false))
            .await
            .unwrap();

        let first = store
            .incoming_after(vec!["/k/bob".into()], 0, 64)
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert!(first[0].0 < first[1].0, "cursors must increase");

        // Resuming from the last id yields nothing — the daemon does not reprocess.
        let resumed = store
            .incoming_after(vec!["/k/bob".into()], first[1].0, 64)
            .await
            .unwrap();
        assert!(resumed.is_empty());

        // …and a message arriving after that point is picked up from the same cursor.
        store
            .enqueue(msg("m3", "/k/bob", "/k/alice", false))
            .await
            .unwrap();
        let next = store
            .incoming_after(vec!["/k/bob".into()], first[1].0, 64)
            .await
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].1.id, "m3");
    }

    /// Echoing a mailbox's own sent copies back would wake an agent to read its own words.
    #[tokio::test]
    async fn sent_copies_never_appear_in_the_stream() {
        let store = Store::open(":memory:").unwrap();
        store
            .enqueue(msg("in", "/k/bob", "/k/alice", false))
            .await
            .unwrap();
        store
            .enqueue(msg("out", "/k/bob", "/k/bob", true))
            .await
            .unwrap();

        let rows = store
            .incoming_after(vec!["/k/bob".into()], 0, 64)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.id, "in");
    }

    /// One stream serves a whole account, so it must span that account's mailboxes and no others.
    #[tokio::test]
    async fn the_stream_spans_the_account_and_stops_there() {
        let store = Store::open(":memory:").unwrap();
        store
            .enqueue(msg("a", "/k/one", "/k/x", false))
            .await
            .unwrap();
        store
            .enqueue(msg("b", "/k/two", "/k/x", false))
            .await
            .unwrap();
        store
            .enqueue(msg("c", "/k/other", "/k/x", false))
            .await
            .unwrap();

        let rows = store
            .incoming_after(vec!["/k/one".into(), "/k/two".into()], 0, 64)
            .await
            .unwrap();
        let ids: Vec<_> = rows.iter().map(|(_, m)| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a", "b"],
            "another account's mail must not leak in"
        );
    }

    /// A daemon connecting for the first time wants what happens next, not a replay of history.
    #[tokio::test]
    async fn a_fresh_stream_starts_after_existing_mail() {
        let store = Store::open(":memory:").unwrap();
        assert_eq!(store.latest_message_cursor().await.unwrap(), 0);
        store
            .enqueue(msg("old", "/k/bob", "/k/alice", false))
            .await
            .unwrap();

        let start = store.latest_message_cursor().await.unwrap();
        assert!(start > 0);
        assert!(store
            .incoming_after(vec!["/k/bob".into()], start, 64)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_batch_is_bounded_so_a_long_sleep_catches_up_in_chunks() {
        let store = Store::open(":memory:").unwrap();
        for i in 0..10 {
            store
                .enqueue(msg(&format!("m{i}"), "/k/bob", "/k/alice", false))
                .await
                .unwrap();
        }
        let rows = store
            .incoming_after(vec!["/k/bob".into()], 0, 4)
            .await
            .unwrap();
        assert_eq!(rows.len(), 4);
    }
}
