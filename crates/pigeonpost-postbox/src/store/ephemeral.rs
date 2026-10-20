//! Disappearing direct messages (SDS §17): a timer per thread, an expiry per message, and the
//! sweep that deletes what has expired.
//!
//! Both sides of a direct conversation carry the same thread id, so one `UPDATE … WHERE id = ?`
//! writes the timer to both mailboxes' thread rows in one statement.

use super::{events, params, Store, StoreError};
use std::collections::HashMap;

pub(super) const MIGRATIONS: &[&str] = &[
    "ALTER TABLE threads ADD COLUMN ephemeral_secs INTEGER",
    "ALTER TABLE messages ADD COLUMN expires_at INTEGER",
    "CREATE INDEX IF NOT EXISTS messages_expiry ON messages(expires_at) WHERE expires_at IS NOT NULL",
];

/// What one expiry pass removed from direct threads, per account to notify.
#[derive(Debug, Default)]
pub struct DmExpiry {
    /// `(account, mailbox, thread_id, message ids)`.
    pub expired: Vec<(Option<String>, String, String, Vec<String>)>,
    /// Attachment digests nothing refers to any more.
    pub blobs: Vec<String>,
}

impl Store {
    /// The timer on a thread, as the given mailbox sees it, or `None` when there is no such
    /// thread in that mailbox.
    pub async fn thread_timer(
        &self,
        owner: String,
        thread_id: String,
    ) -> Result<Option<Option<u64>>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Option<u64>>, StoreError> {
            let c = conn.lock().expect("store lock");
            let row: Option<Option<i64>> = c
                .query_row(
                    "SELECT ephemeral_secs FROM threads WHERE id = ?1 AND owner = ?2",
                    params![thread_id, owner],
                    |r| r.get(0),
                )
                .ok();
            Ok(row.map(|v| v.map(|s| s.max(0) as u64)))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Every timer set on `owner`'s threads.
    pub async fn thread_timers(&self, owner: String) -> Result<HashMap<String, u64>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<HashMap<String, u64>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT id, ephemeral_secs FROM threads WHERE owner = ?1 AND ephemeral_secs IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![owner], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            let mut out = HashMap::new();
            for row in rows {
                let (id, secs) = row?;
                out.insert(id, secs.max(0) as u64);
            }
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Set the timer on both sides of a thread and announce it to both mailboxes' accounts, in one
    /// transaction. Returns the mailboxes holding the thread, or `None` when `owner` holds no
    /// thread with this id.
    pub async fn set_thread_timer(
        &self,
        owner: String,
        thread_id: String,
        secs: Option<u64>,
        changed_by: String,
        now: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Vec<String>>, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let mine: i64 = tx.query_row(
                "SELECT COUNT(*) FROM threads WHERE id = ?1 AND owner = ?2",
                params![thread_id, owner],
                |r| r.get(0),
            )?;
            if mine == 0 {
                return Ok(None);
            }
            tx.execute(
                "UPDATE threads SET ephemeral_secs = ?2 WHERE id = ?1",
                params![thread_id, secs.map(|s| s as i64)],
            )?;
            let holders: Vec<(String, Option<String>)> = {
                let mut stmt = tx.prepare(
                    "SELECT t.owner, i.account_id FROM threads t
                       LEFT JOIN identities i ON i.address = t.owner
                      WHERE t.id = ?1",
                )?;
                let rows = stmt.query_map(params![thread_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            let data = serde_json::json!({
                "thread_id": thread_id,
                "ephemeral_secs": secs,
                "changed_by": changed_by,
            });
            for (mailbox, account) in &holders {
                if let Some(account) = account {
                    events::append_in(&tx, account, "thread.settings", Some(mailbox), &data, now)?;
                }
            }
            tx.commit()?;
            Ok(Some(holders.into_iter().map(|(m, _)| m).collect()))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Stamp an expiry on stored message copies (both sides of one send).
    pub async fn set_message_expiry(
        &self,
        ids: Vec<String>,
        expires_at: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            for id in ids {
                tx.execute(
                    "UPDATE messages SET expires_at = ?2 WHERE id = ?1",
                    params![id, expires_at as i64],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Expiry times of the given message copies, for list responses.
    pub async fn message_expiries(
        &self,
        ids: Vec<String>,
    ) -> Result<HashMap<String, u64>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<HashMap<String, u64>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut out = HashMap::new();
            let mut stmt = c.prepare(
                "SELECT expires_at FROM messages WHERE id = ?1 AND expires_at IS NOT NULL",
            )?;
            for id in ids {
                if let Ok(at) = stmt.query_row(params![id], |r| r.get::<_, i64>(0)) {
                    out.insert(id, at.max(0) as u64);
                }
            }
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Delete up to `batch` expired direct-message copies, release their attachments and record a
    /// `message.expired` event per (mailbox, thread). Idempotent and restartable.
    pub async fn expire_direct_messages(
        &self,
        now: u64,
        batch: usize,
    ) -> Result<DmExpiry, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<DmExpiry, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let rows: Vec<(String, String, Option<String>)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, COALESCE(owner, recipient), thread_id FROM messages
                      WHERE expires_at IS NOT NULL AND expires_at <= ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![now as i64, batch as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
                rows.collect::<Result<_, _>>()?
            };
            let mut out = DmExpiry::default();
            if rows.is_empty() {
                return Ok(out);
            }
            let mut grouped: HashMap<(String, String), Vec<String>> = HashMap::new();
            let mut touched = Vec::new();
            for (id, owner, thread) in &rows {
                {
                    let mut stmt = tx.prepare(
                        "SELECT sha256 FROM attachments WHERE message_id = ?1 AND owner = ?2",
                    )?;
                    let shas = stmt.query_map(params![id, owner], |r| r.get::<_, String>(0))?;
                    for sha in shas {
                        touched.push(sha?);
                    }
                }
                tx.execute(
                    "DELETE FROM attachments WHERE message_id = ?1 AND owner = ?2",
                    params![id, owner],
                )?;
                tx.execute(
                    "DELETE FROM messages WHERE id = ?1 AND COALESCE(owner, recipient) = ?2",
                    params![id, owner],
                )?;
                grouped
                    .entry((owner.clone(), thread.clone().unwrap_or_default()))
                    .or_default()
                    .push(id.clone());
            }
            touched.sort();
            touched.dedup();
            for sha in touched {
                let still: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM attachments WHERE sha256 = ?1",
                    params![sha],
                    |r| r.get(0),
                )?;
                if still == 0 {
                    out.blobs.push(sha);
                }
            }
            for ((owner, thread), ids) in grouped {
                let account: Option<String> = tx
                    .query_row(
                        "SELECT account_id FROM identities WHERE address = ?1",
                        params![owner],
                        |r| r.get(0),
                    )
                    .ok()
                    .flatten();
                if let Some(account) = &account {
                    events::append_in(
                        &tx,
                        account,
                        "message.expired",
                        Some(&owner),
                        &serde_json::json!({ "thread_id": thread, "ids": ids }),
                        now,
                    )?;
                }
                out.expired.push((account, owner, thread, ids));
            }
            tx.commit()?;
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}
