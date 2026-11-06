//! Filtering settings and the relationship facts they are decided on (SDS §13).
//!
//! The settings live in `inbox_policy`, next to the two stranger-defaults that were already there,
//! because they answer the same question for different channels: what happens to someone this
//! mailbox has not vouched for. They are read at delivery (messages) and at ring time (calls), so a
//! change takes effect on the next message or call, on every device at once.

use super::{namespace_wildcard, params, OptionalExtension, Store, StoreError};

/// Columns added to existing tables. Run after the base schema; "duplicate column name" on a fresh
/// database is benign, as for the other migrations.
pub(super) const MIGRATIONS: &[&str] = &[
    "ALTER TABLE inbox_policy ADD COLUMN unknown_calls TEXT NOT NULL DEFAULT 'ring'",
    "ALTER TABLE inbox_policy ADD COLUMN unnamed_calls TEXT NOT NULL DEFAULT 'silence'",
    "ALTER TABLE inbox_policy ADD COLUMN unnamed_messages TEXT NOT NULL DEFAULT 'show'",
    "ALTER TABLE inbox_policy ADD COLUMN unknown_messages TEXT NOT NULL DEFAULT 'show'",
    "ALTER TABLE inbox_policy ADD COLUMN low_reputation TEXT NOT NULL DEFAULT 'filter'",
    "ALTER TABLE inbox_policy ADD COLUMN group_members_known INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE inbox_policy ADD COLUMN call_history_days INTEGER NOT NULL DEFAULT 180",
    // 1 once the owner chose `unknown_calls`. Until then the column's 'ring' is only the column
    // default, and the effective value is the mailbox kind's default (SDS §13.1: agents Silence).
    "ALTER TABLE inbox_policy ADD COLUMN unknown_calls_set INTEGER NOT NULL DEFAULT 0",
    // Delivered but filed in the recipient's Filtered list.
    "ALTER TABLE messages ADD COLUMN filtered INTEGER NOT NULL DEFAULT 0",
    "CREATE INDEX IF NOT EXISTS messages_filtered ON messages(owner, sender) WHERE filtered = 1",
    // "Has this mailbox ever written to that one" is asked on every filtered decision.
    "CREATE INDEX IF NOT EXISTS messages_outgoing_pair ON messages(owner, recipient) WHERE direction = 'out'",
];

/// What happens to a call a rule applies to. Ordered by strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CallRule {
    Ring,
    Silence,
    Block,
}

/// What happens to a message a rule applies to. Ordered by strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MessageRule {
    Show,
    Filter,
    Block,
}

/// Low-reputation senders. Ordered by strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReputationRule {
    Allow,
    Filter,
}

impl CallRule {
    pub fn as_str(self) -> &'static str {
        match self {
            CallRule::Ring => "ring",
            CallRule::Silence => "silence",
            CallRule::Block => "block",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ring" => CallRule::Ring,
            "silence" => CallRule::Silence,
            "block" => CallRule::Block,
            _ => return None,
        })
    }
}

impl MessageRule {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageRule::Show => "show",
            MessageRule::Filter => "filter",
            MessageRule::Block => "block",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "show" => MessageRule::Show,
            "filter" => MessageRule::Filter,
            "block" => MessageRule::Block,
            _ => return None,
        })
    }
}

impl ReputationRule {
    pub fn as_str(self) -> &'static str {
        match self {
            ReputationRule::Allow => "allow",
            ReputationRule::Filter => "filter",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "allow" => ReputationRule::Allow,
            "filter" => ReputationRule::Filter,
            _ => return None,
        })
    }
}

/// The "Keep call history" choices, in days. 0 is Off.
pub const CALL_HISTORY_CHOICES: [u32; 4] = [0, 30, 180, 365];
pub const CALL_HISTORY_DEFAULT_DAYS: u32 = 180;

/// One mailbox's filtering settings (SDS §13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filters {
    pub unknown_calls: CallRule,
    pub unnamed_calls: CallRule,
    pub unnamed_messages: MessageRule,
    pub unknown_messages: MessageRule,
    pub low_reputation: ReputationRule,
    /// Shared private-group members count as known. Groups do not exist yet; the setting is stored
    /// so clients can show it, and is consulted once they do.
    pub group_members_known: bool,
    pub call_history_days: u32,
}

impl Filters {
    /// The defaults for an agent mailbox: as for a person, except that calls from people the
    /// mailbox does not know are silenced (SDS §13.1). Nobody is at the other end to answer them.
    pub fn agent_default() -> Self {
        Filters {
            unknown_calls: CallRule::Silence,
            ..Filters::default()
        }
    }

    /// The defaults for `address`, by mailbox kind.
    fn default_for(agent: bool) -> Self {
        if agent {
            Filters::agent_default()
        } else {
            Filters::default()
        }
    }
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            unknown_calls: CallRule::Ring,
            unnamed_calls: CallRule::Silence,
            unnamed_messages: MessageRule::Show,
            unknown_messages: MessageRule::Show,
            low_reputation: ReputationRule::Filter,
            group_members_known: false,
            call_history_days: CALL_HISTORY_DEFAULT_DAYS,
        }
    }
}

/// A partial settings write. `None` keeps the stored value.
#[derive(Debug, Clone, Copy, Default)]
pub struct FiltersUpdate {
    pub unknown_calls: Option<CallRule>,
    pub unnamed_calls: Option<CallRule>,
    pub unnamed_messages: Option<MessageRule>,
    pub unknown_messages: Option<MessageRule>,
    pub low_reputation: Option<ReputationRule>,
    pub group_members_known: Option<bool>,
    pub call_history_days: Option<u32>,
}

impl FiltersUpdate {
    pub fn is_empty(&self) -> bool {
        self.unknown_calls.is_none()
            && self.unnamed_calls.is_none()
            && self.unnamed_messages.is_none()
            && self.unknown_messages.is_none()
            && self.low_reputation.is_none()
            && self.group_members_known.is_none()
            && self.call_history_days.is_none()
    }

    /// The first field that would make `current` *less* strict, if any. Agents may only tighten.
    pub fn loosens(&self, current: &Filters) -> Option<&'static str> {
        if self
            .unknown_calls
            .is_some_and(|v| v < current.unknown_calls)
        {
            return Some("unknown_calls");
        }
        if self
            .unnamed_calls
            .is_some_and(|v| v < current.unnamed_calls)
        {
            return Some("unnamed_calls");
        }
        if self
            .unnamed_messages
            .is_some_and(|v| v < current.unnamed_messages)
        {
            return Some("unnamed_messages");
        }
        if self
            .unknown_messages
            .is_some_and(|v| v < current.unknown_messages)
        {
            return Some("unknown_messages");
        }
        if self
            .low_reputation
            .is_some_and(|v| v < current.low_reputation)
        {
            return Some("low_reputation");
        }
        // Counting more people as known widens who gets through.
        if self.group_members_known == Some(true) && !current.group_members_known {
            return Some("group_members_known");
        }
        // Keeping more history is keeping more about other people.
        if self
            .call_history_days
            .is_some_and(|v| v > current.call_history_days)
        {
            return Some("call_history_days");
        }
        None
    }
}

/// How the owner of a mailbox stands with one sender or caller, from the owner's own records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Relationship {
    /// A contact row with `admission = block` governs this peer.
    pub blocked: bool,
    /// A contact row with `admission = allow` governs this peer.
    pub allowed: bool,
    /// The owner has written to this peer before (an outgoing copy exists).
    pub messaged: bool,
}

impl Relationship {
    /// "People you know" (SDS §13.1): an allowed contact or someone the owner wrote to first.
    /// Org rules and opted-in private groups join here when they exist.
    pub fn known(&self) -> bool {
        !self.blocked && (self.allowed || self.messaged)
    }
}

fn read_filters(row: &rusqlite::Row<'_>, d: Filters) -> rusqlite::Result<Filters> {
    let text = |i: usize| -> rusqlite::Result<String> { row.get::<_, String>(i) };
    Ok(Filters {
        unknown_calls: if row.get::<_, i64>(7)? != 0 {
            CallRule::parse(&text(0)?).unwrap_or(d.unknown_calls)
        } else {
            d.unknown_calls
        },
        unnamed_calls: CallRule::parse(&text(1)?).unwrap_or(d.unnamed_calls),
        unnamed_messages: MessageRule::parse(&text(2)?).unwrap_or(d.unnamed_messages),
        unknown_messages: MessageRule::parse(&text(3)?).unwrap_or(d.unknown_messages),
        low_reputation: ReputationRule::parse(&text(4)?).unwrap_or(d.low_reputation),
        group_members_known: row.get::<_, i64>(5)? != 0,
        call_history_days: {
            let days = row.get::<_, i64>(6)?;
            u32::try_from(days)
                .ok()
                .filter(|d| CALL_HISTORY_CHOICES.contains(d))
                .unwrap_or(d.call_history_days)
        },
    })
}

const FILTER_COLS: &str = "unknown_calls, unnamed_calls, unnamed_messages, unknown_messages, \
                           low_reputation, group_members_known, call_history_days";

/// Whether `address` is an agent mailbox: an org agent (`org_agents`, any status but deleted), or a
/// personal agent handle under a person's name (`/namespace/person/agent`, three or more segments,
/// the shape `authorize_open_handle` treats as an agent).
pub(crate) fn is_agent_mailbox_in(
    c: &rusqlite::Connection,
    address: &str,
) -> Result<bool, StoreError> {
    let org_agent = c
        .query_row(
            "SELECT 1 FROM org_agents WHERE identity = ?1 AND status != 'deleted'",
            params![address],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if org_agent {
        return Ok(true);
    }
    let handle: Option<String> = c
        .query_row(
            "SELECT handle FROM identities WHERE address = ?1",
            params![address],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(handle.is_some_and(|h| agent_handle(&h)))
}

/// `/namespace/person/agent` (or deeper): the handle names an agent under a person.
fn agent_handle(handle: &str) -> bool {
    handle
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .count()
        > 2
}

/// The settings, for callers already holding the lock. Fields the owner never chose take the
/// default for the mailbox's kind.
pub(super) fn filters_in(c: &rusqlite::Connection, address: &str) -> Result<Filters, StoreError> {
    let d = Filters::default_for(is_agent_mailbox_in(c, address)?);
    Ok(c.query_row(
        &format!("SELECT {FILTER_COLS}, unknown_calls_set FROM inbox_policy WHERE address = ?1"),
        params![address],
        |r| read_filters(r, d),
    )
    .optional()?
    .unwrap_or(d))
}

impl Store {
    /// One mailbox's filtering settings, or the defaults when it never set any.
    pub async fn filters(&self, address: String) -> Result<Filters, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Filters, StoreError> {
            let c = conn.lock().expect("store lock");
            filters_in(&c, &address)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Apply a partial settings write and return the result. Read and write happen under one lock,
    /// so two concurrent writes of different fields cannot lose each other.
    pub async fn set_filters(
        &self,
        address: String,
        update: FiltersUpdate,
        now: u64,
    ) -> Result<Filters, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Filters, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let cur = filters_in(&tx, &address)?;
            let next = Filters {
                unknown_calls: update.unknown_calls.unwrap_or(cur.unknown_calls),
                unnamed_calls: update.unnamed_calls.unwrap_or(cur.unnamed_calls),
                unnamed_messages: update.unnamed_messages.unwrap_or(cur.unnamed_messages),
                unknown_messages: update.unknown_messages.unwrap_or(cur.unknown_messages),
                low_reputation: update.low_reputation.unwrap_or(cur.low_reputation),
                group_members_known: update
                    .group_members_known
                    .unwrap_or(cur.group_members_known),
                call_history_days: update.call_history_days.unwrap_or(cur.call_history_days),
            };
            // A row created here takes the stranger-defaults' own column defaults, which are the
            // same values `InboxPolicy::default` reports.
            tx.execute(
                &format!(
                    "INSERT INTO inbox_policy (address, updated_at, {FILTER_COLS}, unknown_calls_set)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(address) DO UPDATE SET
                        updated_at = ?2, unknown_calls = ?3, unnamed_calls = ?4,
                        unnamed_messages = ?5, unknown_messages = ?6, low_reputation = ?7,
                        group_members_known = ?8, call_history_days = ?9,
                        unknown_calls_set = MAX(unknown_calls_set, ?10)"
                ),
                params![
                    address,
                    now as i64,
                    next.unknown_calls.as_str(),
                    next.unnamed_calls.as_str(),
                    next.unnamed_messages.as_str(),
                    next.unknown_messages.as_str(),
                    next.low_reputation.as_str(),
                    next.group_members_known as i64,
                    next.call_history_days as i64,
                    update.unknown_calls.is_some() as i64,
                ],
            )?;
            tx.commit()?;
            Ok(next)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Whether `owner` blocks any of `peers` (`(address, handle)`), by the same rule as
    /// [`Store::relationship`]: exact rows outrank a namespace wildcard, and between exact rows a
    /// block wins. One pass over every peer, however many mailboxes an account has (audit
    /// 2026-11-03 P3-4: contact discovery used to check only the first 50).
    pub async fn blocks_any(
        &self,
        owner: String,
        peers: Vec<(String, Option<String>)>,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare_cached(
                "SELECT peer, admission FROM contacts
                  WHERE owner = ?1 AND (peer = ?2 OR peer = ?3 OR peer = ?4)",
            )?;
            for (address, handle) in peers {
                let wildcard = handle.as_deref().and_then(namespace_wildcard);
                let rows: Vec<(String, String)> = stmt
                    .query_map(params![owner, address, handle, wildcard], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                let exact: Vec<&str> = rows
                    .iter()
                    .filter(|(peer, _)| Some(peer) != wildcard.as_ref())
                    .map(|(_, a)| a.as_str())
                    .collect();
                let governing: Vec<&str> = if exact.is_empty() {
                    rows.iter().map(|(_, a)| a.as_str()).collect()
                } else {
                    exact
                };
                if governing.contains(&"block") {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// How `owner` stands with the mailbox at `peer_address` (which answers to `peer_handle`, if
    /// it has one).
    ///
    /// Exact rows — the `/k/` address or the handle — outrank a namespace wildcard, as everywhere
    /// else. Between two exact rows a block wins: a person who blocked the key address of someone
    /// now writing under a handle has blocked that someone.
    pub async fn relationship(
        &self,
        owner: String,
        peer_address: String,
        peer_handle: Option<String>,
    ) -> Result<Relationship, StoreError> {
        let wildcard = peer_handle.as_deref().and_then(namespace_wildcard);
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Relationship, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT peer, admission FROM contacts
                  WHERE owner = ?1 AND (peer = ?2 OR peer = ?3 OR peer = ?4)",
            )?;
            let rows: Vec<(String, String)> = stmt
                .query_map(params![owner, peer_address, peer_handle, wildcard], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let exact: Vec<&str> = rows
                .iter()
                .filter(|(peer, _)| Some(peer) != wildcard.as_ref())
                .map(|(_, a)| a.as_str())
                .collect();
            let governing: Vec<&str> = if exact.is_empty() {
                rows.iter().map(|(_, a)| a.as_str()).collect()
            } else {
                exact
            };
            let blocked = governing.contains(&"block");
            let allowed = !blocked && governing.contains(&"allow");
            let messaged: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM messages
                                 WHERE owner = ?1 AND direction = 'out' AND recipient = ?2)",
                params![owner, peer_address],
                |r| r.get::<_, i64>(0).map(|n| n != 0),
            )?;
            Ok(Relationship {
                blocked,
                allowed,
                messaged,
            })
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Move `sender`'s filtered mail into `owner`'s inbox. Returns how many messages moved.
    pub async fn unfilter_from(&self, owner: String, sender: String) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.execute(
                "UPDATE messages SET filtered = 0
                  WHERE owner = ?1 AND sender = ?2 AND direction = 'in' AND filtered = 1",
                params![owner, sender],
            )?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strictness_is_ordered() {
        assert!(CallRule::Ring < CallRule::Silence && CallRule::Silence < CallRule::Block);
        assert!(
            MessageRule::Show < MessageRule::Filter && MessageRule::Filter < MessageRule::Block
        );
        assert!(ReputationRule::Allow < ReputationRule::Filter);
    }

    #[test]
    fn agents_may_only_tighten() {
        let cur = Filters::default();
        let tighten = FiltersUpdate {
            unknown_calls: Some(CallRule::Block),
            unknown_messages: Some(MessageRule::Filter),
            call_history_days: Some(30),
            ..FiltersUpdate::default()
        };
        assert_eq!(tighten.loosens(&cur), None);
        for (update, field) in [
            (
                FiltersUpdate {
                    unnamed_calls: Some(CallRule::Ring),
                    ..FiltersUpdate::default()
                },
                "unnamed_calls",
            ),
            (
                FiltersUpdate {
                    low_reputation: Some(ReputationRule::Allow),
                    ..FiltersUpdate::default()
                },
                "low_reputation",
            ),
            (
                FiltersUpdate {
                    group_members_known: Some(true),
                    ..FiltersUpdate::default()
                },
                "group_members_known",
            ),
            (
                FiltersUpdate {
                    call_history_days: Some(365),
                    ..FiltersUpdate::default()
                },
                "call_history_days",
            ),
        ] {
            assert_eq!(update.loosens(&cur), Some(field));
        }
    }

    #[tokio::test]
    async fn settings_round_trip_and_keep_the_stranger_defaults() {
        let store = Store::open(":memory:").unwrap();
        assert_eq!(
            store.filters("/k/a".into()).await.unwrap(),
            Filters::default()
        );
        let set = store
            .set_filters(
                "/k/a".into(),
                FiltersUpdate {
                    unknown_messages: Some(MessageRule::Filter),
                    call_history_days: Some(0),
                    ..FiltersUpdate::default()
                },
                10,
            )
            .await
            .unwrap();
        assert_eq!(set.unknown_messages, MessageRule::Filter);
        assert_eq!(set.call_history_days, 0);
        assert_eq!(set.unnamed_calls, CallRule::Silence);
        // The row the filters created must not have closed the inbox or raised autonomy.
        assert_eq!(
            store.inbox_policy("/k/a".into()).await.unwrap(),
            crate::store::InboxPolicy::default()
        );
        // And the stranger-defaults writer must not reset the filters.
        store
            .set_inbox_policy("/k/a".into(), Some(false), None, 11)
            .await
            .unwrap();
        assert_eq!(store.filters("/k/a".into()).await.unwrap(), set);
    }

    #[tokio::test]
    async fn exact_rows_outrank_the_wildcard_and_a_block_wins_between_exact_rows() {
        let store = Store::open(":memory:").unwrap();
        let put = |peer: &str, admission: &str| crate::store::ContactUpdate {
            owner: "/k/me".into(),
            peer: peer.into(),
            alias: None,
            admission: Some(admission.into()),
            autonomy: None,
            allowed_verbs: None,
            now: 1,
        };
        store.upsert_contact(put("/acme/*", "block")).await.unwrap();
        let r = store
            .relationship("/k/me".into(), "/k/x".into(), Some("/acme/bot".into()))
            .await
            .unwrap();
        assert!(r.blocked && !r.known());
        // An exact allow on the handle outranks the namespace block.
        store
            .upsert_contact(put("/acme/bot", "allow"))
            .await
            .unwrap();
        let r = store
            .relationship("/k/me".into(), "/k/x".into(), Some("/acme/bot".into()))
            .await
            .unwrap();
        assert!(!r.blocked && r.allowed && r.known());
        // A block on the key address wins over the allow on the handle.
        store.upsert_contact(put("/k/x", "block")).await.unwrap();
        let r = store
            .relationship("/k/me".into(), "/k/x".into(), Some("/acme/bot".into()))
            .await
            .unwrap();
        assert!(r.blocked && !r.known());
        // A stranger is neither.
        let r = store
            .relationship("/k/me".into(), "/k/y".into(), None)
            .await
            .unwrap();
        assert_eq!(r, Relationship::default());
    }

    /// Mailboxes inserted directly: (address, handle).
    fn seed(store: &Store, rows: &[(&str, Option<&str>)]) {
        let c = store.conn.lock().unwrap();
        for (address, handle) in rows {
            c.execute(
                "INSERT INTO identities (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct,
                                         cap_hash, created_at, handle)
                 VALUES (?1, x'00', x'00', x'00', x'00', x'00', 1, ?2)",
                params![address, handle],
            )
            .unwrap();
        }
    }

    #[test]
    fn agent_handles_are_under_a_person() {
        assert!(agent_handle("/ada/main/build"));
        assert!(agent_handle("/ada/main/build/sub"));
        assert!(!agent_handle("/ada/main"));
        assert!(!agent_handle("/ada"));
        assert!(!agent_handle("/k/abc"));
    }

    #[tokio::test]
    async fn agent_mailboxes_silence_unknown_callers_until_their_owner_chooses() {
        let store = Store::open(":memory:").unwrap();
        seed(
            &store,
            &[
                ("/k/agent", Some("/ada/main/build")),
                ("/k/person", Some("/ada/main")),
                ("/k/orgagent", Some("/acme/build")),
                ("/k/plain", None),
            ],
        );
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO org_agents (org_id, identity, handle, created_at)
                 VALUES ('org_1', '/k/orgagent', '/acme/build', 1)",
                [],
            )
            .unwrap();
        let calls = |a: &'static str| {
            let store = &store;
            async move { store.filters(a.into()).await.unwrap().unknown_calls }
        };
        assert_eq!(calls("/k/agent").await, CallRule::Silence);
        assert_eq!(calls("/k/orgagent").await, CallRule::Silence);
        assert_eq!(calls("/k/person").await, CallRule::Ring);
        assert_eq!(calls("/k/plain").await, CallRule::Ring);
        assert_eq!(
            store.filters("/k/agent".into()).await.unwrap(),
            Filters::agent_default()
        );

        // A row created by another setting (here the stranger-defaults writer, and a filters write
        // of another field) does not turn the column default into a choice.
        store
            .set_inbox_policy("/k/agent".into(), Some(false), None, 2)
            .await
            .unwrap();
        let other = store
            .set_filters(
                "/k/agent".into(),
                FiltersUpdate {
                    low_reputation: Some(ReputationRule::Allow),
                    ..FiltersUpdate::default()
                },
                3,
            )
            .await
            .unwrap();
        assert_eq!(other.unknown_calls, CallRule::Silence);
        assert_eq!(calls("/k/agent").await, CallRule::Silence);

        // An explicit Ring is kept, also across later writes of other fields.
        let set = store
            .set_filters(
                "/k/agent".into(),
                FiltersUpdate {
                    unknown_calls: Some(CallRule::Ring),
                    ..FiltersUpdate::default()
                },
                4,
            )
            .await
            .unwrap();
        assert_eq!(set.unknown_calls, CallRule::Ring);
        store
            .set_filters(
                "/k/agent".into(),
                FiltersUpdate {
                    call_history_days: Some(30),
                    ..FiltersUpdate::default()
                },
                5,
            )
            .await
            .unwrap();
        assert_eq!(calls("/k/agent").await, CallRule::Ring);

        // A deleted org agent is no longer an agent mailbox.
        store
            .conn
            .lock()
            .unwrap()
            .execute("UPDATE org_agents SET status = 'deleted'", [])
            .unwrap();
        assert_eq!(calls("/k/orgagent").await, CallRule::Ring);
    }
}
