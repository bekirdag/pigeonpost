//! Durable account events (SDS §3.5): an outbox written in the same transaction as the state
//! change it announces, read back by `GET /v1/events`.
//!
//! # Cursor
//!
//! `/v1/events` already delivers `mail` events whose SSE id is the message rowid, and shipped
//! clients persist that number (agentd's cursor file, the Windows app parses it as an integer). The
//! typed log therefore shares one ordering with mail instead of starting a second id space:
//!
//! * every typed event records `mail_hwm`, the highest message rowid that existed when it
//!   committed (never decreasing, even if that message is later deleted);
//! * a mail event sits at position `(rowid, 0)`, a typed event at `(mail_hwm, id)`;
//! * the stream emits both in that order, and a cursor is the position of the last thing seen.
//!
//! A legacy integer cursor `N` is `(N, 0)`; a typed event's id is written `"<mail_hwm>.<id>"`.
//! Old clients keep sending integers and never lose mail; new clients resume exactly.
//!
//! # Retention
//!
//! Typed events are kept for a bounded window (age and count per account). Pruning records the
//! newest pruned position per account, so a cursor from before it can be answered with
//! `resync_required` instead of a silent gap.

use super::{params, Connection, Store, StoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS account_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    mail_hwm   INTEGER NOT NULL,
    type       TEXT NOT NULL,
    mailbox    TEXT,
    data       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS account_events_by_account ON account_events(account_id, id);
CREATE INDEX IF NOT EXISTS account_events_by_age ON account_events(created_at);
-- The newest position pruned from each account's log: anything at or before it is gone.
CREATE TABLE IF NOT EXISTS account_event_floor (
    account_id TEXT PRIMARY KEY,
    mail_hwm   INTEGER NOT NULL,
    id         INTEGER NOT NULL
);
-- The running high-water mark, so it never moves backwards when the newest message is deleted.
CREATE TABLE IF NOT EXISTS account_event_meta (
    k TEXT PRIMARY KEY,
    v INTEGER NOT NULL
);
-- Phase 9: message rowids are never reused. `messages` has no AUTOINCREMENT, so SQLite gives a new
-- row MAX(rowid)+1 — the rowid of a just-deleted newest message. A client whose cursor already
-- passed that rowid (it saw the deleted message) would never be told about the new one. `mail_seq`
-- is the highest rowid ever inserted; a new row at or below it is moved just above it.
INSERT OR IGNORE INTO account_event_meta (k, v)
    SELECT 'mail_seq', MAX(COALESCE((SELECT MAX(rowid) FROM messages), 0),
                           COALESCE((SELECT v FROM account_event_meta WHERE k = 'mail_hwm'), 0));
CREATE TRIGGER IF NOT EXISTS messages_rowid_never_reused AFTER INSERT ON messages
BEGIN
    UPDATE messages
       SET rowid = (SELECT v FROM account_event_meta WHERE k = 'mail_seq') + 1
     WHERE rowid = NEW.rowid
       AND NEW.rowid <= COALESCE((SELECT v FROM account_event_meta WHERE k = 'mail_seq'), 0);
    INSERT INTO account_event_meta (k, v) VALUES ('mail_seq', (SELECT MAX(rowid) FROM messages))
        ON CONFLICT(k) DO UPDATE SET v = MAX(v, excluded.v);
END;
";

/// A position in an account's merged mail + typed-event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct StreamCursor {
    pub mail: i64,
    pub event: i64,
}

impl StreamCursor {
    /// `"123"` (a legacy mail cursor) or `"123.45"` (a typed-event cursor). Anything else is not
    /// a cursor.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (mail, event) = match raw.split_once('.') {
            Some((m, e)) => (m.parse().ok()?, e.parse().ok()?),
            None => (raw.parse().ok()?, 0),
        };
        (mail >= 0 && event >= 0).then_some(StreamCursor { mail, event })
    }

    /// The SSE id of a typed event at this position.
    pub fn typed_id(self) -> String {
        format!("{}.{}", self.mail, self.event)
    }
}

/// One typed event as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEvent {
    pub cursor: StreamCursor,
    pub kind: String,
    pub mailbox: Option<String>,
    pub data: String,
    pub created_at: u64,
}

/// Append a typed event inside the caller's transaction. Returns its position.
pub(super) fn append_in(
    c: &Connection,
    account: &str,
    kind: &str,
    mailbox: Option<&str>,
    data: &serde_json::Value,
    now: u64,
) -> Result<StreamCursor, StoreError> {
    let messages: i64 = c.query_row("SELECT COALESCE(MAX(rowid), 0) FROM messages", [], |r| {
        r.get(0)
    })?;
    let previous: i64 = c
        .query_row(
            "SELECT v FROM account_event_meta WHERE k = 'mail_hwm'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // Never below the highest rowid ever inserted either: a deleted newest message must not let a
    // typed event sort behind a mail cursor that already passed it.
    let seq: i64 = c
        .query_row(
            "SELECT v FROM account_event_meta WHERE k = 'mail_seq'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let hwm = messages.max(previous).max(seq);
    c.execute(
        "INSERT INTO account_event_meta (k, v) VALUES ('mail_hwm', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![hwm],
    )?;
    c.execute(
        "INSERT INTO account_events (account_id, mail_hwm, type, mailbox, data, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![account, hwm, kind, mailbox, data.to_string(), now as i64],
    )?;
    Ok(StreamCursor {
        mail: hwm,
        event: c.last_insert_rowid(),
    })
}

/// What one read of the merged stream produced.
#[derive(Default)]
pub struct StreamBatch {
    /// Incoming mail after the cursor: `(rowid, message)`.
    pub mail: Vec<(i64, super::Message)>,
    /// Typed events after the cursor (empty unless typed events were asked for).
    pub typed: Vec<AccountEvent>,
    /// The cursor is older than the retained window: events were pruned that it never saw.
    pub resync: bool,
    /// A limit was hit; read again before waiting.
    pub more: bool,
}

impl Store {
    /// Record a typed event on its own, for a change already committed (or living elsewhere).
    pub async fn record_account_event(
        &self,
        account: String,
        kind: String,
        mailbox: Option<String>,
        data: serde_json::Value,
        now: u64,
    ) -> Result<StreamCursor, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<StreamCursor, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let at = append_in(&tx, &account, &kind, mailbox.as_deref(), &data, now)?;
            tx.commit()?;
            Ok(at)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record a typed event on its own (for changes whose state lives outside this database).
    #[cfg(test)]
    pub async fn append_account_event(
        &self,
        account: String,
        kind: String,
        mailbox: Option<String>,
        data: serde_json::Value,
        now: u64,
    ) -> Result<StreamCursor, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<StreamCursor, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let at = append_in(&tx, &account, &kind, mailbox.as_deref(), &data, now)?;
            tx.commit()?;
            Ok(at)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Where a stream opened without a cursor starts: after everything that exists now.
    pub async fn stream_head(&self, account: String) -> Result<StreamCursor, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<StreamCursor, StoreError> {
            let c = conn.lock().expect("store lock");
            let mail: i64 =
                c.query_row("SELECT COALESCE(MAX(rowid), 0) FROM messages", [], |r| {
                    r.get(0)
                })?;
            let event: i64 = c.query_row(
                "SELECT COALESCE(MAX(id), 0) FROM account_events WHERE account_id = ?1",
                params![account],
                |r| r.get(0),
            )?;
            // The typed position is (hwm, id) of the newest event; mail after it is still newer.
            let hwm: i64 = c
                .query_row(
                    "SELECT mail_hwm FROM account_events WHERE id = ?1",
                    params![event],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(if hwm >= mail {
                StreamCursor { mail: hwm, event }
            } else {
                StreamCursor { mail, event: 0 }
            })
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Everything after `cursor` for this account, mail and (when `typed`) typed events, read under
    /// one lock so the two halves agree on what has committed. Each half is capped at `limit`;
    /// the result is trimmed so nothing past the shorter half's end is emitted out of order.
    pub async fn stream_after(
        &self,
        account: String,
        owners: Vec<String>,
        cursor: StreamCursor,
        typed: bool,
        limit: usize,
    ) -> Result<StreamBatch, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<StreamBatch, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut batch = StreamBatch {
                mail: super::incoming_after_in(&c, &owners, cursor.mail, limit)?,
                ..StreamBatch::default()
            };
            if typed {
                let floor: Option<(i64, i64)> = c
                    .query_row(
                        "SELECT mail_hwm, id FROM account_event_floor WHERE account_id = ?1",
                        params![account],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                if let Some((mail, event)) = floor {
                    batch.resync = cursor < StreamCursor { mail, event };
                }
                let mut stmt = c.prepare(
                    "SELECT id, mail_hwm, type, mailbox, data, created_at FROM account_events
                     WHERE account_id = ?1
                       AND (mail_hwm > ?2 OR (mail_hwm = ?2 AND id > ?3))
                     ORDER BY id ASC LIMIT ?4",
                )?;
                let rows = stmt.query_map(
                    params![account, cursor.mail, cursor.event, limit as i64],
                    |r| {
                        Ok(AccountEvent {
                            cursor: StreamCursor {
                                event: r.get(0)?,
                                mail: r.get(1)?,
                            },
                            kind: r.get(2)?,
                            mailbox: r.get(3)?,
                            data: r.get(4)?,
                            created_at: r.get::<_, i64>(5)? as u64,
                        })
                    },
                )?;
                batch.typed = rows.collect::<Result<_, _>>()?;
            }

            // A full half may have more behind it. Anything in the other half positioned after the
            // full half's last item must wait for the next read, or it would overtake those.
            let mail_full = batch.mail.len() >= limit;
            let typed_full = batch.typed.len() >= limit;
            if mail_full {
                if let Some(&(last, _)) = batch.mail.last() {
                    let edge = StreamCursor {
                        mail: last,
                        event: 0,
                    };
                    batch.typed.retain(|e| e.cursor < edge);
                }
            }
            if typed_full {
                if let Some(edge) = batch.typed.last().map(|e| e.cursor) {
                    batch.mail.retain(|(rowid, _)| {
                        StreamCursor {
                            mail: *rowid,
                            event: 0,
                        } < edge
                    });
                }
            }
            batch.more = mail_full || typed_full;
            Ok(batch)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Drop typed events older than `cutoff` and beyond `max_per_account` per account, recording
    /// each account's newest pruned position. Returns how many rows were removed.
    pub async fn prune_account_events(
        &self,
        cutoff: u64,
        max_per_account: usize,
    ) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            // The newest row each account is about to lose, by age…
            let mut doomed: Vec<(String, i64, i64)> = {
                let mut stmt = tx.prepare(
                    "SELECT e.account_id, e.mail_hwm, e.id FROM account_events e
                     JOIN (SELECT account_id, MAX(id) AS id FROM account_events
                           WHERE created_at < ?1 GROUP BY account_id) d ON d.id = e.id",
                )?;
                let rows = stmt.query_map(params![cutoff as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
                rows.collect::<Result<_, _>>()?
            };
            // …and by count.
            {
                let mut over = tx.prepare(
                    "SELECT account_id FROM account_events GROUP BY account_id HAVING COUNT(*) > ?1",
                )?;
                let accounts: Vec<String> = over
                    .query_map(params![max_per_account as i64], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for account in accounts {
                    let edge: (i64, i64) = tx.query_row(
                        "SELECT mail_hwm, id FROM account_events WHERE account_id = ?1
                         ORDER BY id DESC LIMIT 1 OFFSET ?2",
                        params![account, max_per_account as i64],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    doomed.push((account, edge.0, edge.1));
                }
            }
            let mut removed = 0;
            for (account, hwm, id) in doomed {
                tx.execute(
                    "INSERT INTO account_event_floor (account_id, mail_hwm, id) VALUES (?1, ?2, ?3)
                     ON CONFLICT(account_id) DO UPDATE SET mail_hwm = excluded.mail_hwm,
                         id = excluded.id
                     WHERE excluded.id > account_event_floor.id",
                    params![account, hwm, id],
                )?;
                removed += tx.execute(
                    "DELETE FROM account_events WHERE account_id = ?1 AND id <= ?2",
                    params![account, id],
                )?;
            }
            tx.commit()?;
            Ok(removed)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Message;

    fn msg(id: &str, owner: &str) -> Message {
        Message {
            thread_id: "t".into(),
            filtered: false,
            id: id.into(),
            recipient: owner.into(),
            sender: "/k/alice".into(),
            wrap_blob: vec![1],
            created_at: 0,
            read: false,
            owner: owner.into(),
            outgoing: false,
        }
    }

    fn ev(n: u64) -> serde_json::Value {
        serde_json::json!({ "n": n })
    }

    #[test]
    fn cursors_parse_legacy_and_typed_forms() {
        assert_eq!(
            StreamCursor::parse("17"),
            Some(StreamCursor { mail: 17, event: 0 })
        );
        assert_eq!(
            StreamCursor::parse("17.4"),
            Some(StreamCursor { mail: 17, event: 4 })
        );
        for bad in ["", "x", "1.x", "-1", "1.-2", "1.2.3"] {
            assert_eq!(StreamCursor::parse(bad), None, "{bad}");
        }
        assert_eq!(StreamCursor { mail: 3, event: 9 }.typed_id(), "3.9");
    }

    #[tokio::test]
    async fn mail_and_typed_events_interleave_in_commit_order_and_resume_exactly() {
        let store = Store::open(":memory:").unwrap();
        let owners = vec!["/k/bob".to_string()];
        store.enqueue(msg("m1", "/k/bob")).await.unwrap();
        let e1 = store
            .append_account_event("acct".into(), "x.one".into(), None, ev(1), 10)
            .await
            .unwrap();
        store.enqueue(msg("m2", "/k/bob")).await.unwrap();
        let e2 = store
            .append_account_event("acct".into(), "x.two".into(), None, ev(2), 11)
            .await
            .unwrap();
        // Another account's events never appear.
        store
            .append_account_event("other".into(), "x.no".into(), None, ev(0), 11)
            .await
            .unwrap();

        let all = store
            .stream_after(
                "acct".into(),
                owners.clone(),
                StreamCursor::default(),
                true,
                64,
            )
            .await
            .unwrap();
        assert_eq!(all.mail.len(), 2);
        assert_eq!(all.typed.len(), 2);
        assert!(!all.resync && !all.more);
        let m1 = all.mail[0].0;
        let m2 = all.mail[1].0;
        // m1 < e1 < m2 < e2 in stream order.
        assert!(StreamCursor { mail: m1, event: 0 } < e1);
        assert!(e1 < StreamCursor { mail: m2, event: 0 });
        assert!(StreamCursor { mail: m2, event: 0 } < e2);

        // Resuming from e1's typed id yields m2 and e2 only.
        let after_e1 = store
            .stream_after("acct".into(), owners.clone(), e1, true, 64)
            .await
            .unwrap();
        assert_eq!(after_e1.mail.len(), 1);
        assert_eq!(after_e1.mail[0].0, m2);
        assert_eq!(after_e1.typed.len(), 1);
        assert_eq!(after_e1.typed[0].kind, "x.two");

        // A legacy integer cursor at m2 still gets e2 (committed after m2) and nothing twice.
        let legacy = StreamCursor::parse(&m2.to_string()).unwrap();
        let after_m2 = store
            .stream_after("acct".into(), owners.clone(), legacy, true, 64)
            .await
            .unwrap();
        assert!(after_m2.mail.is_empty());
        assert_eq!(after_m2.typed.len(), 1);

        // With typed events off, the stream is mail only.
        let mail_only = store
            .stream_after("acct".into(), owners, StreamCursor::default(), false, 64)
            .await
            .unwrap();
        assert_eq!(mail_only.mail.len(), 2);
        assert!(mail_only.typed.is_empty());
    }

    #[tokio::test]
    async fn hwm_never_moves_backwards_when_the_newest_message_is_deleted() {
        let store = Store::open(":memory:").unwrap();
        store.enqueue(msg("m1", "/k/bob")).await.unwrap();
        store.enqueue(msg("m2", "/k/bob")).await.unwrap();
        let a = store
            .append_account_event("acct".into(), "t".into(), None, ev(1), 1)
            .await
            .unwrap();
        store
            .delete_message("/k/bob".into(), "m2".into())
            .await
            .unwrap();
        let b = store
            .append_account_event("acct".into(), "t".into(), None, ev(2), 2)
            .await
            .unwrap();
        assert!(b > a);
        assert!(b.mail >= a.mail);
    }

    #[tokio::test]
    async fn a_full_batch_does_not_let_the_other_half_overtake_it() {
        let store = Store::open(":memory:").unwrap();
        let owners = vec!["/k/bob".to_string()];
        for i in 0..3 {
            store
                .append_account_event("acct".into(), "t".into(), None, ev(i), 1)
                .await
                .unwrap();
        }
        store.enqueue(msg("m-late", "/k/bob")).await.unwrap();
        let first = store
            .stream_after(
                "acct".into(),
                owners.clone(),
                StreamCursor::default(),
                true,
                2,
            )
            .await
            .unwrap();
        assert!(first.more);
        assert_eq!(first.typed.len(), 2);
        assert!(first.mail.is_empty(), "the later mail must wait its turn");
        let next = store
            .stream_after("acct".into(), owners, first.typed[1].cursor, true, 2)
            .await
            .unwrap();
        assert_eq!(next.typed.len(), 1);
        assert_eq!(next.mail.len(), 1);
    }

    #[tokio::test]
    async fn pruning_bounds_the_log_and_old_cursors_must_resync() {
        let store = Store::open(":memory:").unwrap();
        let owners: Vec<String> = Vec::new();
        let old = store
            .append_account_event("acct".into(), "t".into(), None, ev(0), 100)
            .await
            .unwrap();
        let mut last = old;
        for i in 1..=5 {
            last = store
                .append_account_event("acct".into(), "t".into(), None, ev(i), 1_000)
                .await
                .unwrap();
        }
        // Age prunes the first; the count cap of 3 prunes two more.
        let removed = store.prune_account_events(500, 3).await.unwrap();
        assert_eq!(removed, 3);

        let from_start = store
            .stream_after(
                "acct".into(),
                owners.clone(),
                StreamCursor::default(),
                true,
                64,
            )
            .await
            .unwrap();
        assert!(
            from_start.resync,
            "a cursor older than the window must resync"
        );
        assert_eq!(from_start.typed.len(), 3);

        let current = store
            .stream_after("acct".into(), owners, last, true, 64)
            .await
            .unwrap();
        assert!(!current.resync);
        assert!(current.typed.is_empty());

        // Pruning again with nothing to drop leaves the floor where it was.
        assert_eq!(store.prune_account_events(500, 3).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn stream_head_starts_after_everything_that_exists() {
        let store = Store::open(":memory:").unwrap();
        let owners = vec!["/k/bob".to_string()];
        store.enqueue(msg("m1", "/k/bob")).await.unwrap();
        store
            .append_account_event("acct".into(), "t".into(), None, ev(1), 1)
            .await
            .unwrap();
        store.enqueue(msg("m2", "/k/bob")).await.unwrap();
        let head = store.stream_head("acct".into()).await.unwrap();
        let batch = store
            .stream_after("acct".into(), owners, head, true, 64)
            .await
            .unwrap();
        assert!(batch.mail.is_empty() && batch.typed.is_empty());
    }
}
