//! Call history (SDS §12): one row per participant mailbox per call, folded from the lifecycle
//! events the calls service reports.
//!
//! # Folding
//!
//! Events arrive at least once and not necessarily in order (the calls service retries). Each
//! carries a stable `event_id`, so a retry is recognised and ignored. Outcomes move through a
//! monotonic state machine: an in-progress row (`outcome IS NULL`) takes any outcome; a terminal
//! outcome is only ever replaced by `answered` (an answer that arrived after a reordered "missed");
//! `answered` is never replaced. A late `ring` therefore cannot undo an answer or a hang-up.
//!
//! # Deletion
//!
//! Deleting an entry keeps a tombstone (`deleted_at`) for the replay window so a retried event for
//! the same call finds the row and leaves it alone instead of recreating it; the reaper purges
//! tombstones afterwards. Clearing the whole history also records `cleared_at`, which refuses rows
//! for calls that started before it.

use super::{
    events, features, filtering, params, Connection, OptionalExtension, Store, StoreError,
};

/// The feature flag that gates recording (kept in step with `capabilities::CALL_HISTORY`).
pub const CALL_HISTORY_FLAG: &str = "call_history";

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS call_log (
    id                TEXT PRIMARY KEY,
    owner             TEXT NOT NULL,
    room_id           TEXT NOT NULL,
    calendar_event_id TEXT,
    occurrence_id     TEXT,
    direction         TEXT CHECK (direction IN ('outgoing','incoming')),
    outcome           TEXT CHECK (outcome IN ('answered','missed','declined','cancelled','busy','filtered','failed')),
    kind              TEXT CHECK (kind IN ('audio','video')),
    peer              TEXT,
    peer_handle       TEXT,
    group_id          TEXT,
    participants      TEXT,
    started_at        INTEGER NOT NULL,
    answered_at       INTEGER,
    ended_at          INTEGER,
    seen              INTEGER NOT NULL DEFAULT 0,
    deleted_at        INTEGER,
    last_event_version INTEGER NOT NULL DEFAULT 0,
    UNIQUE (owner, room_id)
);
CREATE INDEX IF NOT EXISTS call_log_owner ON call_log(owner, started_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS call_log_room ON call_log(room_id);
-- Event ids already folded, for the retry window.
CREATE TABLE IF NOT EXISTS call_events_seen (
    event_id    TEXT PRIMARY KEY,
    received_at INTEGER NOT NULL
);
-- When a mailbox last cleared its whole history: calls that started before it are not recreated.
CREATE TABLE IF NOT EXISTS call_log_cleared (
    owner      TEXT PRIMARY KEY,
    cleared_at INTEGER NOT NULL
);
";

/// How long dedup records and deletion tombstones are kept: longer than the calls service retries.
pub const REPLAY_WINDOW_SECS: u64 = 7 * 86_400;
/// Participants kept per entry.
const MAX_PARTICIPANTS: usize = 50;

/// What happened, as the calls service reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEventKind {
    Ring,
    Answer,
    Join,
    Decline,
    Busy,
    Missed,
    Cancelled,
    Filtered,
    End,
}

impl CallEventKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ring" => Self::Ring,
            "answer" => Self::Answer,
            "join" => Self::Join,
            "decline" => Self::Decline,
            "busy" => Self::Busy,
            "missed" => Self::Missed,
            "cancelled" => Self::Cancelled,
            "filtered" => Self::Filtered,
            "end" => Self::End,
            _ => return None,
        })
    }
}

/// One lifecycle event, validated.
#[derive(Debug, Clone)]
pub struct CallEvent {
    pub event_id: String,
    pub room_id: String,
    pub seq: i64,
    pub kind: String,
    pub event: CallEventKind,
    /// The caller (room creator), by `/k/` address or handle.
    pub from: String,
    /// Who the caller dialled, by address or handle. Present even when nobody was rung.
    pub callee: Option<String>,
    /// The mailboxes this event is about (rung, missed, filtered, declined).
    pub to: Vec<String>,
    /// Who answered, joined or declined.
    pub by: Option<String>,
    /// Addresses, or `guest:<name>` for guests without a mailbox.
    pub participants: Vec<String>,
    pub at: u64,
    /// The scheduled meeting this room belongs to (Phase 3B), if any.
    pub calendar_event_id: Option<String>,
    pub occurrence_id: Option<String>,
}

/// One history entry, as the API returns it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CallLogEntry {
    pub id: String,
    pub owner: String,
    pub room_id: String,
    pub direction: String,
    pub outcome: Option<String>,
    pub kind: String,
    pub peer: Option<String>,
    pub peer_handle: Option<String>,
    pub participants: Vec<String>,
    pub group_id: Option<String>,
    pub calendar_event_id: Option<String>,
    pub occurrence_id: Option<String>,
    pub started_at: u64,
    pub answered_at: Option<u64>,
    pub ended_at: Option<u64>,
    pub duration: Option<u64>,
    pub seen: bool,
}

/// Which entries a listing wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilter {
    /// Everything except filtered calls, which have their own section.
    All,
    /// Incoming calls nobody answered.
    Missed,
    Filtered,
}

/// What folding one event did.
#[derive(Debug, Default)]
pub struct FoldResult {
    pub duplicate: bool,
    /// `(account, entry)` for every entry that changed, already recorded as `call.log` events.
    pub changed: Vec<(String, CallLogEntry)>,
}

const ENTRY_COLS: &str = "id, owner, room_id, direction, outcome, kind, peer, peer_handle, \
                          participants, group_id, started_at, answered_at, ended_at, seen, \
                          calendar_event_id, occurrence_id";

fn read_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<CallLogEntry> {
    let started_at = r.get::<_, i64>(10)? as u64;
    let answered_at = r.get::<_, Option<i64>>(11)?.map(|v| v as u64);
    let ended_at = r.get::<_, Option<i64>>(12)?.map(|v| v as u64);
    Ok(CallLogEntry {
        id: r.get(0)?,
        owner: r.get(1)?,
        room_id: r.get(2)?,
        direction: r.get(3)?,
        outcome: r.get(4)?,
        kind: r.get(5)?,
        peer: r.get(6)?,
        peer_handle: r.get(7)?,
        participants: r
            .get::<_, Option<String>>(8)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        group_id: r.get(9)?,
        started_at,
        answered_at,
        ended_at,
        duration: match (answered_at, ended_at) {
            (Some(a), Some(e)) => Some(e.saturating_sub(a)),
            _ => None,
        },
        seen: r.get::<_, i64>(13)? != 0,
        calendar_event_id: r.get(14)?,
        occurrence_id: r.get(15)?,
    })
}

/// A hosted mailbox: `(address, handle, account)`.
type Mailbox = (String, Option<String>, Option<String>);

/// A hosted mailbox, by address or handle.
fn mailbox(c: &Connection, name: &str) -> Result<Option<Mailbox>, StoreError> {
    Ok(c.query_row(
        "SELECT address, handle, account_id FROM identities
          WHERE address = ?1 OR handle = lower(?1) LIMIT 1",
        params![name],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?)
}

fn new_id() -> String {
    let mut b = [0u8; 13];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
    let mut s = String::from("clg_");
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

struct Fold<'a> {
    c: &'a Connection,
    ev: &'a CallEvent,
    touched: Vec<String>,
}

/// Outcomes, ranked for the monotonic rule.
fn may_replace(current: Option<&str>, next: &str) -> bool {
    match current {
        None => true,
        Some("answered") => false,
        Some(_) => next == "answered",
    }
}

impl Fold<'_> {
    /// The row for `owner` in this room, created if the owner keeps history. `None` when the owner
    /// is not hosted here, keeps no history, deleted the entry or cleared history since.
    fn row(
        &mut self,
        owner: &str,
        direction: &str,
        peer: Option<&Mailbox>,
    ) -> Result<Option<String>, StoreError> {
        let Some((address, _, account)) = mailbox(self.c, owner)? else {
            return Ok(None);
        };
        let existing: Option<(String, Option<i64>)> = self
            .c
            .query_row(
                "SELECT id, deleted_at FROM call_log WHERE owner = ?1 AND room_id = ?2",
                params![address, self.ev.room_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, deleted)) = existing {
            return Ok(deleted.is_none().then_some(id));
        }
        // Nothing is recorded for an account the feature is not on for, and nothing for a mailbox
        // that turned history off.
        if !features::enabled_in(self.c, CALL_HISTORY_FLAG, account.as_deref())?
            || filtering::filters_in(self.c, &address)?.call_history_days == 0
        {
            return Ok(None);
        }
        let cleared: Option<i64> = self
            .c
            .query_row(
                "SELECT cleared_at FROM call_log_cleared WHERE owner = ?1",
                params![address],
                |r| r.get(0),
            )
            .optional()?;
        if cleared.is_some_and(|t| self.ev.at as i64 <= t) {
            return Ok(None);
        }
        let id = new_id();
        self.c.execute(
            "INSERT INTO call_log (id, owner, room_id, direction, kind, peer, peer_handle,
                                   started_at, seen, last_event_version, calendar_event_id,
                                   occurrence_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                address,
                self.ev.room_id,
                direction,
                self.ev.kind,
                peer.map(|p| p.0.clone()),
                peer.and_then(|p| p.1.clone()),
                self.ev.at as i64,
                // Only an incoming missed call is news; everything else starts seen.
                (direction == "outgoing") as i64,
                self.ev.seq,
                self.ev.calendar_event_id,
                self.ev.occurrence_id,
            ],
        )?;
        self.touch(&id);
        Ok(Some(id))
    }

    fn touch(&mut self, id: &str) {
        if !self.touched.iter().any(|t| t == id) {
            self.touched.push(id.to_string());
        }
    }

    fn set_outcome(&mut self, id: &str, outcome: &str) -> Result<(), StoreError> {
        let current: Option<String> = self.c.query_row(
            "SELECT outcome FROM call_log WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if !may_replace(current.as_deref(), outcome) || current.as_deref() == Some(outcome) {
            return Ok(());
        }
        self.c.execute(
            "UPDATE call_log SET outcome = ?2,
                    seen = CASE WHEN ?2 = 'missed' AND direction = 'incoming' THEN 0 ELSE 1 END,
                    answered_at = CASE WHEN ?2 = 'answered' THEN COALESCE(answered_at, ?3)
                                       ELSE answered_at END,
                    last_event_version = MAX(last_event_version, ?4)
              WHERE id = ?1",
            params![id, outcome, self.ev.at as i64, self.ev.seq],
        )?;
        self.touch(id);
        Ok(())
    }

    fn room_rows(&self) -> Result<Vec<(String, String, Option<String>)>, StoreError> {
        let mut stmt = self.c.prepare(
            "SELECT id, direction, outcome FROM call_log
              WHERE room_id = ?1 AND deleted_at IS NULL",
        )?;
        let rows = stmt
            .query_map(params![self.ev.room_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    fn set_participants(&mut self) -> Result<(), StoreError> {
        if self.ev.participants.is_empty() {
            return Ok(());
        }
        let list: Vec<&String> = self.ev.participants.iter().take(MAX_PARTICIPANTS).collect();
        let json = serde_json::to_string(&list).expect("strings serialize");
        for (id, _, _) in self.room_rows()? {
            let n = self.c.execute(
                "UPDATE call_log SET participants = ?2 WHERE id = ?1
                    AND COALESCE(participants, '') != ?2",
                params![id, json],
            )?;
            if n > 0 {
                self.touch(&id);
            }
        }
        Ok(())
    }
}

fn fold(c: &Connection, ev: &CallEvent, now: u64) -> Result<FoldResult, StoreError> {
    let fresh = c.execute(
        "INSERT OR IGNORE INTO call_events_seen (event_id, received_at) VALUES (?1, ?2)",
        params![ev.event_id, now as i64],
    )?;
    if fresh == 0 {
        return Ok(FoldResult {
            duplicate: true,
            changed: Vec::new(),
        });
    }
    let caller = mailbox(c, &ev.from)?;
    let callee = match ev.callee.as_deref().or(ev.to.first().map(String::as_str)) {
        Some(name) => mailbox(c, name)?,
        None => None,
    };
    let mut f = Fold {
        c,
        ev,
        touched: Vec::new(),
    };
    let caller_row = |f: &mut Fold| -> Result<Option<String>, StoreError> {
        match caller.as_ref() {
            Some(me) => f.row(&me.0, "outgoing", callee.as_ref()),
            None => Ok(None),
        }
    };
    let callee_row = |f: &mut Fold, name: &str| -> Result<Option<String>, StoreError> {
        f.row(name, "incoming", caller.as_ref())
    };
    // The callee rows this event names, never the caller's own mailbox.
    let targets: Vec<String> = ev
        .to
        .iter()
        .filter(|t| {
            caller
                .as_ref()
                .is_none_or(|me| &me.0 != *t && me.1.as_ref() != Some(t))
        })
        .cloned()
        .collect();

    match ev.event {
        CallEventKind::Ring => {
            caller_row(&mut f)?;
            for t in &targets {
                callee_row(&mut f, t)?;
            }
        }
        CallEventKind::Answer | CallEventKind::Join => {
            // The caller joining their own room (a scheduled meeting's organizer) is not an
            // incoming call to them.
            let by_caller = |b: &str| {
                caller
                    .as_ref()
                    .is_some_and(|me| me.0 == b || me.1.as_deref() == Some(b))
            };
            if let Some(by) = ev.by.as_deref().filter(|b| !by_caller(b)) {
                if let Some(id) = callee_row(&mut f, by)? {
                    f.set_outcome(&id, "answered")?;
                }
            }
            if let Some(id) = caller_row(&mut f)? {
                f.set_outcome(&id, "answered")?;
            }
            f.set_participants()?;
        }
        CallEventKind::Decline | CallEventKind::Busy => {
            let outcome = if ev.event == CallEventKind::Busy {
                "busy"
            } else {
                "declined"
            };
            let whose: Vec<String> = ev.by.iter().cloned().chain(targets.clone()).collect();
            for t in &whose {
                if let Some(id) = callee_row(&mut f, t)? {
                    f.set_outcome(&id, outcome)?;
                }
            }
            if let Some(id) = caller_row(&mut f)? {
                f.set_outcome(&id, outcome)?;
            }
        }
        CallEventKind::Missed => {
            for t in &targets {
                if let Some(id) = callee_row(&mut f, t)? {
                    f.set_outcome(&id, "missed")?;
                }
            }
            if let Some(id) = caller_row(&mut f)? {
                f.set_outcome(&id, "missed")?;
            }
        }
        CallEventKind::Cancelled => {
            if let Some(id) = caller_row(&mut f)? {
                f.set_outcome(&id, "cancelled")?;
            }
            for (id, direction, outcome) in f.room_rows()? {
                if direction == "incoming" && outcome.is_none() {
                    f.set_outcome(&id, "missed")?;
                }
            }
        }
        CallEventKind::Filtered => {
            for t in &targets {
                if let Some(id) = callee_row(&mut f, t)? {
                    f.set_outcome(&id, "filtered")?;
                }
            }
        }
        CallEventKind::End => {
            f.set_participants()?;
            for (id, direction, outcome) in f.room_rows()? {
                match outcome.as_deref() {
                    None => {
                        let terminal = if direction == "outgoing" {
                            "cancelled"
                        } else {
                            "missed"
                        };
                        f.set_outcome(&id, terminal)?;
                    }
                    Some("answered") => {
                        let n = c.execute(
                            "UPDATE call_log SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
                            params![id, ev.at as i64],
                        )?;
                        if n > 0 {
                            f.touch(&id);
                        }
                    }
                    Some(_) => {}
                }
            }
        }
    }

    let mut changed = Vec::new();
    for id in &f.touched {
        let entry = c.query_row(
            &format!("SELECT {ENTRY_COLS} FROM call_log WHERE id = ?1"),
            params![id],
            read_entry,
        )?;
        let account: Option<String> = c
            .query_row(
                "SELECT account_id FROM identities WHERE address = ?1",
                params![entry.owner],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(account) = account {
            let data = serde_json::to_value(&entry).expect("entry serializes");
            events::append_in(c, &account, "call.log", Some(&entry.owner), &data, now)?;
            changed.push((account, entry));
        }
    }
    Ok(FoldResult {
        duplicate: false,
        changed,
    })
}

impl Store {
    /// Fold one lifecycle event into every affected mailbox's history, in one transaction.
    pub async fn apply_call_event(
        &self,
        ev: CallEvent,
        now: u64,
    ) -> Result<FoldResult, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<FoldResult, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let out = fold(&tx, &ev, now)?;
            tx.commit()?;
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// One page of `owner`'s history, newest first, and how many missed calls are unseen.
    pub async fn call_log(
        &self,
        owner: String,
        filter: LogFilter,
        before: Option<u64>,
        limit: usize,
    ) -> Result<(Vec<CallLogEntry>, usize), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(Vec<CallLogEntry>, usize), StoreError> {
            let c = conn.lock().expect("store lock");
            let condition = match filter {
                LogFilter::All => "(outcome IS NULL OR outcome != 'filtered')",
                LogFilter::Missed => "direction = 'incoming' AND outcome = 'missed'",
                LogFilter::Filtered => "outcome = 'filtered'",
            };
            let mut stmt = c.prepare(&format!(
                "SELECT {ENTRY_COLS} FROM call_log
                  WHERE owner = ?1 AND deleted_at IS NULL AND {condition}
                    AND (?2 IS NULL OR started_at < ?2)
                  ORDER BY started_at DESC, id DESC LIMIT ?3"
            ))?;
            let rows = stmt
                .query_map(
                    params![owner, before.map(|b| b as i64), limit as i64],
                    read_entry,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let unseen = unseen_missed_in(&c, &owner)?;
            Ok((rows, unseen))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// One live entry of `owner`'s, by id.
    pub async fn call_entry(
        &self,
        owner: String,
        id: String,
    ) -> Result<Option<CallLogEntry>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<CallLogEntry>, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                &format!(
                    "SELECT {ENTRY_COLS} FROM call_log
                      WHERE owner = ?1 AND id = ?2 AND deleted_at IS NULL"
                ),
                params![owner, id],
                read_entry,
            )
            .optional()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Mark missed calls up to `up_to` seen. Returns `(updated, still_unseen)`.
    pub async fn mark_calls_seen(
        &self,
        owner: String,
        up_to: u64,
    ) -> Result<(usize, usize), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), StoreError> {
            let c = conn.lock().expect("store lock");
            let n = c.execute(
                "UPDATE call_log SET seen = 1
                  WHERE owner = ?1 AND seen = 0 AND started_at <= ?2 AND deleted_at IS NULL",
                params![owner, up_to as i64],
            )?;
            Ok((n, unseen_missed_in(&c, &owner)?))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Delete one entry (tombstoned for the replay window). `false` if it is not `owner`'s.
    pub async fn delete_call_entry(
        &self,
        owner: String,
        id: String,
        now: u64,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.execute(
                "UPDATE call_log SET deleted_at = ?3, participants = NULL, peer = NULL,
                        peer_handle = NULL
                  WHERE owner = ?1 AND id = ?2 AND deleted_at IS NULL",
                params![owner, id, now as i64],
            )? > 0)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Delete every entry of `owner`'s and refuse rows for calls that started before now.
    pub async fn clear_call_log(&self, owner: String, now: u64) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let n = clear_in(&tx, &owner, now)?;
            tx.commit()?;
            Ok(n)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Retention: purge tombstones and dedup records past the replay window, and entries older
    /// than each owner's "Keep call history" setting (Off purges everything).
    pub async fn reap_call_log(&self, now: u64) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let window = now.saturating_sub(REPLAY_WINDOW_SECS) as i64;
            let mut n = tx.execute(
                "DELETE FROM call_log WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
                params![window],
            )?;
            n += tx.execute(
                "DELETE FROM call_log
                  WHERE deleted_at IS NULL AND started_at < ?1 - 86400 * COALESCE(
                        (SELECT p.call_history_days FROM inbox_policy p
                          WHERE p.address = call_log.owner), ?2)",
                params![now as i64, filtering::CALL_HISTORY_DEFAULT_DAYS as i64],
            )?;
            tx.execute(
                "DELETE FROM call_events_seen WHERE received_at < ?1",
                params![window],
            )?;
            tx.execute(
                "DELETE FROM call_log_cleared WHERE cleared_at < ?1",
                params![window],
            )?;
            tx.commit()?;
            Ok(n)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

fn unseen_missed_in(c: &Connection, owner: &str) -> Result<usize, StoreError> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM call_log
          WHERE owner = ?1 AND deleted_at IS NULL AND seen = 0
            AND direction = 'incoming' AND outcome = 'missed'",
        params![owner],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Tombstone every entry and record the clear, inside the caller's transaction.
pub(super) fn clear_in(c: &Connection, owner: &str, now: u64) -> Result<usize, StoreError> {
    let n = c.execute(
        "UPDATE call_log SET deleted_at = ?2, participants = NULL, peer = NULL, peer_handle = NULL
          WHERE owner = ?1 AND deleted_at IS NULL",
        params![owner, now as i64],
    )?;
    c.execute(
        "INSERT INTO call_log_cleared (owner, cleared_at) VALUES (?1, ?2)
         ON CONFLICT(owner) DO UPDATE SET cleared_at = ?2",
        params![owner, now as i64],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoredIdentity;

    async fn store_with(mailboxes: &[(&str, Option<&str>, Option<&str>)]) -> Store {
        let store = Store::open(":memory:").unwrap();
        {
            let c = store.conn.lock().unwrap();
            c.execute(
                "INSERT INTO features (name, scope, scope_id, enabled, updated_at)
                 VALUES ('call_history', 'global', '', 1, 0)",
                [],
            )
            .unwrap();
        }
        for (address, handle, account) in mailboxes {
            let mut id = StoredIdentity::test_fixture(address);
            id.handle = handle.map(str::to_string);
            id.account_id = account.map(str::to_string);
            store.insert(id).await.unwrap();
        }
        store
    }

    fn ev(id: &str, event: CallEventKind, at: u64) -> CallEvent {
        CallEvent {
            event_id: id.into(),
            room_id: "room1".into(),
            seq: 0,
            kind: "video".into(),
            event,
            from: "/k/alice".into(),
            callee: Some("/k/bob".into()),
            to: vec!["/k/bob".into()],
            by: None,
            participants: Vec::new(),
            at,
            calendar_event_id: None,
            occurrence_id: None,
        }
    }

    async fn entry(store: &Store, owner: &str) -> Option<CallLogEntry> {
        let (rows, _) = store
            .call_log(owner.into(), LogFilter::All, None, 10)
            .await
            .unwrap();
        rows.into_iter().next()
    }

    #[tokio::test]
    async fn an_answered_call_is_one_row_per_side_and_survives_reordering() {
        let store = store_with(&[
            ("/k/alice", Some("/a/alice"), Some("acct_a")),
            ("/k/bob", None, Some("acct_b")),
        ])
        .await;
        store
            .apply_call_event(ev("e1", CallEventKind::Ring, 100), 100)
            .await
            .unwrap();
        let mut answer = ev("e2", CallEventKind::Answer, 105);
        answer.by = Some("/k/bob".into());
        answer.participants = vec!["/k/alice".into(), "/k/bob".into()];
        let r = store.apply_call_event(answer.clone(), 105).await.unwrap();
        assert_eq!(r.changed.len(), 2, "both rows changed and were announced");
        // A retry is recognised.
        assert!(store.apply_call_event(answer, 106).await.unwrap().duplicate);
        // A reordered "missed" cannot undo the answer.
        store
            .apply_call_event(ev("e3", CallEventKind::Missed, 145), 145)
            .await
            .unwrap();
        store
            .apply_call_event(ev("e4", CallEventKind::End, 165), 165)
            .await
            .unwrap();
        let bob = entry(&store, "/k/bob").await.unwrap();
        assert_eq!(bob.outcome.as_deref(), Some("answered"));
        assert_eq!(bob.direction, "incoming");
        assert_eq!(bob.peer.as_deref(), Some("/k/alice"));
        assert_eq!(bob.peer_handle.as_deref(), Some("/a/alice"));
        assert_eq!(bob.duration, Some(60));
        assert_eq!(bob.participants.len(), 2);
        let alice = entry(&store, "/k/alice").await.unwrap();
        assert_eq!(alice.direction, "outgoing");
        assert_eq!(alice.outcome.as_deref(), Some("answered"));
        assert_eq!(alice.peer.as_deref(), Some("/k/bob"));
    }

    #[tokio::test]
    async fn a_missed_call_is_unseen_until_marked_and_a_late_ring_changes_nothing() {
        let store = store_with(&[("/k/alice", None, None), ("/k/bob", None, None)]).await;
        store
            .apply_call_event(ev("m1", CallEventKind::Missed, 200), 200)
            .await
            .unwrap();
        store
            .apply_call_event(ev("m0", CallEventKind::Ring, 160), 201)
            .await
            .unwrap();
        let (rows, unseen) = store
            .call_log("/k/bob".into(), LogFilter::Missed, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(unseen, 1);
        assert_eq!(
            entry(&store, "/k/alice").await.unwrap().outcome.as_deref(),
            Some("missed"),
            "the caller's unanswered call reads as missed, as for any no-answer"
        );
        let (n, left) = store.mark_calls_seen("/k/bob".into(), 200).await.unwrap();
        assert_eq!((n, left), (1, 0));
    }

    #[tokio::test]
    async fn a_quiet_ring_writes_only_the_callers_row_and_filtered_writes_only_the_callees() {
        let store = store_with(&[("/k/alice", None, None), ("/k/bob", None, None)]).await;
        let mut ring = ev("q1", CallEventKind::Ring, 300);
        ring.to.clear();
        store.apply_call_event(ring, 300).await.unwrap();
        let mut filtered = ev("q2", CallEventKind::Filtered, 300);
        filtered.callee = None;
        store.apply_call_event(filtered, 300).await.unwrap();
        let mut missed = ev("q3", CallEventKind::Missed, 345);
        missed.to.clear();
        store.apply_call_event(missed, 345).await.unwrap();
        // The callee sees it only in the Filtered section.
        assert!(entry(&store, "/k/bob").await.is_none());
        let (rows, unseen) = store
            .call_log("/k/bob".into(), LogFilter::Filtered, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(unseen, 0);
        let alice = entry(&store, "/k/alice").await.unwrap();
        assert_eq!(alice.outcome.as_deref(), Some("missed"));
        assert_eq!(alice.peer.as_deref(), Some("/k/bob"));
    }

    #[tokio::test]
    async fn deletion_and_clearing_are_not_undone_by_retries_and_off_writes_nothing() {
        let store = store_with(&[("/k/alice", None, None), ("/k/bob", None, None)]).await;
        store
            .apply_call_event(ev("d1", CallEventKind::Ring, 400), 400)
            .await
            .unwrap();
        let bob = entry(&store, "/k/bob").await.unwrap();
        // Not someone else's to delete.
        assert!(!store
            .delete_call_entry("/k/alice".into(), bob.id.clone(), 401)
            .await
            .unwrap());
        assert!(store
            .delete_call_entry("/k/bob".into(), bob.id.clone(), 401)
            .await
            .unwrap());
        store
            .apply_call_event(ev("d2", CallEventKind::Missed, 440), 440)
            .await
            .unwrap();
        assert!(entry(&store, "/k/bob").await.is_none(), "tombstone holds");

        // Clearing refuses a late event for an older call, in a new room id too.
        assert_eq!(
            store.clear_call_log("/k/alice".into(), 500).await.unwrap(),
            1
        );
        let mut late = ev("d3", CallEventKind::Ring, 450);
        late.room_id = "room2".into();
        store.apply_call_event(late, 501).await.unwrap();
        assert!(entry(&store, "/k/alice").await.is_none());

        // Off writes no rows.
        store
            .set_filters(
                "/k/bob".into(),
                filtering::FiltersUpdate {
                    call_history_days: Some(0),
                    ..Default::default()
                },
                600,
            )
            .await
            .unwrap();
        let mut off = ev("d4", CallEventKind::Ring, 600);
        off.room_id = "room3".into();
        store.apply_call_event(off, 600).await.unwrap();
        let (rows, _) = store
            .call_log("/k/bob".into(), LogFilter::All, None, 10)
            .await
            .unwrap();
        assert!(rows.iter().all(|r| r.room_id != "room3"));
    }

    #[tokio::test]
    async fn nothing_is_recorded_while_the_flag_is_off() {
        let store = Store::open(":memory:").unwrap();
        for a in ["/k/alice", "/k/bob"] {
            store.insert(StoredIdentity::test_fixture(a)).await.unwrap();
        }
        store
            .apply_call_event(ev("f1", CallEventKind::Ring, 10), 10)
            .await
            .unwrap();
        assert!(entry(&store, "/k/bob").await.is_none());
        assert!(entry(&store, "/k/alice").await.is_none());
    }

    #[tokio::test]
    async fn retention_follows_each_owners_setting() {
        let store = store_with(&[("/k/alice", None, None), ("/k/bob", None, None)]).await;
        store
            .set_filters(
                "/k/bob".into(),
                filtering::FiltersUpdate {
                    call_history_days: Some(30),
                    ..Default::default()
                },
                0,
            )
            .await
            .unwrap();
        store
            .apply_call_event(ev("r1", CallEventKind::Ring, 1_000), 1_000)
            .await
            .unwrap();
        // 31 days later Bob's 30-day row goes, Alice's default 180-day row stays.
        let later = 1_000 + 31 * 86_400;
        assert_eq!(store.reap_call_log(later).await.unwrap(), 1);
        assert!(entry(&store, "/k/bob").await.is_none());
        assert!(entry(&store, "/k/alice").await.is_some());
    }
}
