//! Message references, edits, replies and receipts (SDS §14C, `docs/message-actions.md`).
//!
//! Kept beside `messages` rather than in it: a direct message is one sealed row per copy, and what
//! this adds — the reference both copies share, what a copy replies to or edits, and the receipt
//! state of a sent copy — is metadata about a row, not part of the sealed mail. A separate table
//! keeps the row shape every existing reader depends on untouched, and a trigger removes the
//! metadata with its row whichever path deletes it (a delete, an expiry, account deletion).

use super::{params, OptionalExtension, Store, StoreError};
use std::collections::HashMap;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS message_meta (
    id              TEXT PRIMARY KEY,
    owner           TEXT NOT NULL,
    msg_ref         TEXT NOT NULL,
    reply_to        TEXT,
    edit_of         TEXT,
    meta_blob       BLOB,
    delivered_at    INTEGER,
    seen_at         INTEGER,
    agent_status    TEXT,
    agent_status_at INTEGER
);
CREATE INDEX IF NOT EXISTS message_meta_by_ref ON message_meta(owner, msg_ref);
CREATE INDEX IF NOT EXISTS message_meta_by_edit ON message_meta(owner, edit_of) WHERE edit_of IS NOT NULL;
CREATE TRIGGER IF NOT EXISTS message_meta_follows_message AFTER DELETE ON messages
BEGIN
    DELETE FROM message_meta WHERE id = OLD.id;
END;
-- How far each group member's clients have fetched, for group delivered ticks.
CREATE TABLE IF NOT EXISTS group_receipts (
    group_id      TEXT NOT NULL,
    member        TEXT NOT NULL,
    delivered_seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, member)
);
CREATE TABLE IF NOT EXISTS receipt_settings (
    mailbox       TEXT PRIMARY KEY,
    read_receipts INTEGER NOT NULL DEFAULT 1,
    updated_at    INTEGER NOT NULL
);
";

/// One row's metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageMeta {
    pub id: String,
    pub owner: String,
    pub msg_ref: String,
    pub reply_to: Option<String>,
    pub edit_of: Option<String>,
    /// A `Wrap` sealed to the owner: `{v, msg_ref, reply_to, edit_of, quote}`.
    pub meta_blob: Option<Vec<u8>>,
    pub delivered_at: Option<u64>,
    pub seen_at: Option<u64>,
    pub agent_status: Option<String>,
    pub agent_status_at: Option<u64>,
}

/// Result of committing both sealed copies and their reference in one transaction.
pub enum DeliveryWrite {
    Stored,
    Duplicate {
        message_id: Option<String>,
        sent_copy_id: String,
    },
    ReferenceConflict,
    ReferenceNotFound,
    EditWindowClosed,
}

/// A copy found by reference, with what the reference rules need to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefRow {
    pub id: String,
    pub msg_ref: String,
    pub outgoing: bool,
    pub sender: String,
    pub recipient: String,
    pub created_at: u64,
    pub thread_id: String,
    pub filtered: bool,
    pub read: bool,
    pub edit_of: Option<String>,
    pub expires_at: Option<u64>,
}

/// Which receipt a write records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptKind {
    Delivered,
    Seen,
}

/// `(msg_ref, sender, filtered, delivered_at, seen_at)` of a received copy.
type ReceivedRow = (String, String, bool, Option<i64>, Option<i64>);

/// `(address, handle, delivered_seq, read_cursor, shares read receipts)` of a group member.
pub type GroupReceiptRow = (String, Option<String>, i64, i64, bool);

fn opt_u64(v: Option<i64>) -> Option<u64> {
    v.map(|n| n.max(0) as u64)
}

fn read_meta(r: &rusqlite::Row<'_>) -> rusqlite::Result<MessageMeta> {
    Ok(MessageMeta {
        id: r.get(0)?,
        owner: r.get(1)?,
        msg_ref: r.get(2)?,
        reply_to: r.get(3)?,
        edit_of: r.get(4)?,
        meta_blob: r.get(5)?,
        delivered_at: opt_u64(r.get(6)?),
        seen_at: opt_u64(r.get(7)?),
        agent_status: r.get(8)?,
        agent_status_at: opt_u64(r.get(9)?),
    })
}

const META_COLUMNS: &str = "id, owner, msg_ref, reply_to, edit_of, meta_blob, delivered_at, \
                            seen_at, agent_status, agent_status_at";

fn read_ref_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RefRow> {
    Ok(RefRow {
        id: r.get(0)?,
        msg_ref: r.get(1)?,
        outgoing: r.get::<_, String>(2)? == "out",
        sender: r.get(3)?,
        recipient: r.get(4)?,
        created_at: r.get::<_, i64>(5)?.max(0) as u64,
        thread_id: r.get(6)?,
        filtered: r.get::<_, i64>(7)? != 0,
        read: r.get::<_, i64>(8)? != 0,
        edit_of: r.get(9)?,
        expires_at: opt_u64(r.get(10)?),
    })
}

const REF_SELECT: &str = "SELECT m.id, mm.msg_ref, m.direction, m.sender, m.recipient, m.created_at,
                                 COALESCE(m.thread_id, ''), m.filtered, m.read, mm.edit_of, m.expires_at
                            FROM messages m JOIN message_meta mm ON mm.id = m.id
                           WHERE COALESCE(m.owner, m.recipient) = ?1
                             AND (m.expires_at IS NULL OR m.expires_at > CAST(strftime('%s','now') AS INTEGER))";

impl Store {
    /// Recheck reference ownership under the writer lock, then atomically persist
    /// both copies, their action metadata and expiry. A polling reader must never
    /// observe an edit as an ordinary message because its metadata is not stored yet.
    pub async fn enqueue_copies(
        &self,
        received: super::Message,
        sent: super::Message,
        metadata: [MessageMeta; 2],
        expires_at: Option<u64>,
        edit_deadline: Option<u64>,
    ) -> Result<DeliveryWrite, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<DeliveryWrite, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let msg_ref = &metadata[0].msg_ref;
            let previous: Option<(String, String)> = tx
                .query_row(
                    "SELECT m.id, m.recipient FROM messages m
                     JOIN message_meta mm ON mm.id = m.id
                     WHERE mm.owner = ?1 AND mm.msg_ref = ?2 AND m.direction = 'out'
                       AND (m.expires_at IS NULL OR m.expires_at > CAST(strftime('%s','now') AS INTEGER))
                     LIMIT 1",
                    params![sent.owner, msg_ref],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((sent_copy_id, recipient)) = previous {
                if recipient != received.recipient {
                    return Ok(DeliveryWrite::ReferenceConflict);
                }
                let message_id = tx
                    .query_row(
                        "SELECT m.id FROM messages m JOIN message_meta mm ON mm.id = m.id
                         WHERE mm.owner = ?1 AND mm.msg_ref = ?2 AND m.direction = 'in'
                           AND m.sender = ?3
                           AND (m.expires_at IS NULL OR m.expires_at > CAST(strftime('%s','now') AS INTEGER))
                         LIMIT 1",
                        params![received.owner, msg_ref, received.sender],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?;
                return Ok(DeliveryWrite::Duplicate { message_id, sent_copy_id });
            }
            let in_use: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_meta mm JOIN messages m ON m.id = mm.id
                 WHERE mm.owner IN (?1, ?2) AND mm.msg_ref = ?3
                   AND (m.expires_at IS NULL OR m.expires_at > CAST(strftime('%s','now') AS INTEGER)))",
                params![received.owner, sent.owner, msg_ref],
                |r| r.get(0),
            )?;
            if in_use {
                return Ok(DeliveryWrite::ReferenceConflict);
            }
            if let Some(root_ref) = &metadata[0].edit_of {
                let root_exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM message_meta mm JOIN messages m ON m.id = mm.id
                     WHERE mm.owner = ?1 AND mm.msg_ref = ?2 AND mm.edit_of IS NULL
                       AND m.direction = 'out' AND m.recipient = ?3
                       AND (m.expires_at IS NULL OR m.expires_at > CAST(strftime('%s','now') AS INTEGER)))",
                    params![sent.owner, root_ref, received.recipient],
                    |r| r.get(0),
                )?;
                if !root_exists {
                    return Ok(DeliveryWrite::ReferenceNotFound);
                }
                let now: i64 = tx.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| r.get(0))?;
                let deadline = edit_deadline.ok_or(StoreError::Corrupt("edit without deadline"))?;
                if now.max(0) as u64 > deadline {
                    return Ok(DeliveryWrite::EditWindowClosed);
                }
            }
            for (m, meta) in [received, sent].into_iter().zip(metadata) {
                if m.id != meta.id || m.owner != meta.owner {
                    return Err(StoreError::Corrupt("message copy metadata mismatch"));
                }
                tx.execute(
                    "INSERT INTO messages
                     (id, recipient, sender, wrap_blob, created_at, read, owner, direction, thread_id, filtered, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![m.id, m.recipient, m.sender, m.wrap_blob, m.created_at as i64,
                        m.read as i64, m.owner, if m.outgoing { "out" } else { "in" },
                        m.thread_id, (m.filtered && !m.outgoing) as i64, expires_at.map(|t| t as i64)],
                )?;
                tx.execute(
                    "INSERT INTO message_meta (id, owner, msg_ref, reply_to, edit_of, meta_blob)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![meta.id, meta.owner, meta.msg_ref, meta.reply_to, meta.edit_of, meta.meta_blob],
                )?;
                tx.execute(
                    "UPDATE threads SET last_at = ?2 WHERE id = ?1 AND last_at < ?2",
                    params![m.thread_id, m.created_at as i64],
                )?;
            }
            tx.commit()?;
            Ok(DeliveryWrite::Stored)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Metadata for some of `owner`'s rows, by row id.
    pub async fn meta_for(
        &self,
        owner: String,
        ids: Vec<String>,
    ) -> Result<HashMap<String, MessageMeta>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> Result<HashMap<String, MessageMeta>, StoreError> {
                let c = conn.lock().expect("store lock");
                let mut out = HashMap::new();
                // Chunked: SQLite caps bound parameters, and a long conversation exceeds it.
                for chunk in ids.chunks(500) {
                    let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                    let sql = format!(
                    "SELECT {META_COLUMNS} FROM message_meta WHERE owner = ? AND id IN ({marks})"
                );
                    let mut stmt = c.prepare(&sql)?;
                    let mut args: Vec<&dyn rusqlite::ToSql> = vec![&owner];
                    for id in chunk {
                        args.push(id);
                    }
                    let rows = stmt.query_map(args.as_slice(), read_meta)?;
                    for row in rows {
                        let row = row?;
                        out.insert(row.id.clone(), row);
                    }
                }
                Ok(out)
            },
        )
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The copies in `owner`'s mailbox that a reference names: by `msg_ref`, or by the row id of
    /// one of `owner`'s own copies. Expired rows are not found. Newest last.
    pub async fn rows_by_ref(&self, owner: String, key: String) -> Result<Vec<RefRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<RefRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            let sql = format!(
                "{REF_SELECT} AND (mm.msg_ref = ?2 OR m.id = ?2) ORDER BY m.created_at ASC"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt
                .query_map(params![owner, key], read_ref_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The edit rows of the message `root` in `owner`'s mailbox, oldest first.
    pub async fn edits_of(&self, owner: String, root: String) -> Result<Vec<RefRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<RefRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            let sql =
                format!("{REF_SELECT} AND mm.edit_of = ?2 ORDER BY m.created_at ASC, m.rowid ASC");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt
                .query_map(params![owner, root], read_ref_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The recipient's copy of a message, found from the sender's side of it.
    pub async fn copy_in(
        &self,
        owner: String,
        msg_ref: String,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                "SELECT id FROM message_meta WHERE owner = ?1 AND msg_ref = ?2 LIMIT 1",
                params![owner, msg_ref],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Mark the recipient-side marker for `kind` on `owner`'s received copies, returning the rows
    /// that changed, as `(row id, msg_ref, sender, filtered)`. `Seen` also implies delivered.
    /// Filtered rows are never marked seen.
    pub async fn mark_received(
        &self,
        owner: String,
        ids: Vec<String>,
        kind: ReceiptKind,
        now: u64,
    ) -> Result<Vec<(String, String, String, bool)>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, String, String, bool)>, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let mut out = Vec::new();
            for id in ids {
                let row: Option<ReceivedRow> = tx
                    .query_row(
                        "SELECT mm.msg_ref, m.sender, m.filtered, mm.delivered_at, mm.seen_at
                           FROM messages m JOIN message_meta mm ON mm.id = m.id
                          WHERE m.id = ?1 AND COALESCE(m.owner, m.recipient) = ?2
                            AND m.direction = 'in'",
                        params![id, owner],
                        |r| {
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                r.get::<_, i64>(2)? != 0,
                                r.get(3)?,
                                r.get(4)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((msg_ref, sender, filtered, delivered, seen)) = row else {
                    continue;
                };
                let changed = match kind {
                    ReceiptKind::Delivered => delivered.is_none(),
                    ReceiptKind::Seen => !filtered && seen.is_none(),
                };
                if !changed {
                    continue;
                }
                match kind {
                    ReceiptKind::Delivered => tx.execute(
                        "UPDATE message_meta SET delivered_at = ?2 WHERE id = ?1",
                        params![id, now as i64],
                    )?,
                    ReceiptKind::Seen => tx.execute(
                        "UPDATE message_meta SET seen_at = ?2, delivered_at = COALESCE(delivered_at, ?2)
                          WHERE id = ?1",
                        params![id, now as i64],
                    )?,
                };
                out.push((id, msg_ref, sender, filtered));
            }
            tx.commit()?;
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Move the sender's copy of `msg_ref` forward. Returns the sender copy's id when its state
    /// changed. States only move forward: delivered never undoes seen.
    pub async fn apply_receipt(
        &self,
        sender: String,
        msg_ref: String,
        kind: ReceiptKind,
        now: u64,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            let id: Option<String> = c
                .query_row(
                    "SELECT mm.id FROM message_meta mm JOIN messages m ON m.id = mm.id
                      WHERE mm.owner = ?1 AND mm.msg_ref = ?2 AND m.direction = 'out'",
                    params![sender, msg_ref],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            let n = match kind {
                ReceiptKind::Delivered => c.execute(
                    "UPDATE message_meta SET delivered_at = ?2 WHERE id = ?1 AND delivered_at IS NULL",
                    params![id, now as i64],
                )?,
                ReceiptKind::Seen => c.execute(
                    "UPDATE message_meta SET seen_at = ?2, delivered_at = COALESCE(delivered_at, ?2)
                      WHERE id = ?1 AND seen_at IS NULL",
                    params![id, now as i64],
                )?,
            };
            Ok((n > 0).then_some(id))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record an agent's run status on the sender's copy. Returns the sender copy's id when it
    /// changed. `working` may follow `done`/`failed` (a follow-up run); a repeat is a no-op.
    pub async fn apply_agent_status(
        &self,
        sender: String,
        msg_ref: String,
        status: &'static str,
        now: u64,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            let id: Option<String> = c
                .query_row(
                    "SELECT mm.id FROM message_meta mm JOIN messages m ON m.id = mm.id
                      WHERE mm.owner = ?1 AND mm.msg_ref = ?2 AND m.direction = 'out'",
                    params![sender, msg_ref],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            let n = c.execute(
                "UPDATE message_meta SET agent_status = ?2, agent_status_at = ?3,
                        delivered_at = COALESCE(delivered_at, ?3)
                  WHERE id = ?1 AND (agent_status IS NULL OR agent_status != ?2)",
                params![id, status, now as i64],
            )?;
            Ok((n > 0).then_some(id))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Whether `mailbox` sends (and sees) read receipts. Default on.
    pub async fn read_receipts(&self, mailbox: String) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                "SELECT read_receipts FROM receipt_settings WHERE mailbox = ?1",
                params![mailbox],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v != 0)
            .unwrap_or(true))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn set_read_receipts(
        &self,
        mailbox: String,
        on: bool,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO receipt_settings (mailbox, read_receipts, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(mailbox) DO UPDATE SET read_receipts = excluded.read_receipts,
                                                    updated_at = excluded.updated_at",
                params![mailbox, on as i64, now as i64],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Mark the edit rows of `root` in `owner`'s mailbox read (an ack of a folded original).
    pub async fn mark_edits_read(
        &self,
        owner: String,
        root: String,
    ) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT m.id FROM messages m JOIN message_meta mm ON mm.id = m.id
                  WHERE COALESCE(m.owner, m.recipient) = ?1 AND mm.edit_of = ?2
                    AND m.direction = 'in' AND m.read = 0",
            )?;
            let ids = stmt
                .query_map(params![owner, root], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            for id in &ids {
                c.execute("UPDATE messages SET read = 1 WHERE id = ?1", params![id])?;
            }
            Ok(ids)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The sealed bodies of some of `owner`'s rows, by id.
    pub async fn wraps_of(
        &self,
        owner: String,
        ids: Vec<String>,
    ) -> Result<HashMap<String, Vec<u8>>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<HashMap<String, Vec<u8>>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut out = HashMap::new();
            for id in ids {
                if let Some(blob) = c
                    .query_row(
                        "SELECT wrap_blob FROM messages WHERE id = ?1 AND COALESCE(owner, recipient) = ?2",
                        params![id, owner],
                        |r| r.get::<_, Vec<u8>>(0),
                    )
                    .optional()?
                {
                    out.insert(id, blob);
                }
            }
            Ok(out)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Advance a group member's delivered cursor (forward only).
    pub async fn note_group_delivered(
        &self,
        group_id: String,
        member: String,
        seq: i64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO group_receipts (group_id, member, delivered_seq) VALUES (?1, ?2, ?3)
                 ON CONFLICT(group_id, member) DO UPDATE
                   SET delivered_seq = MAX(delivered_seq, excluded.delivered_seq)",
                params![group_id, member, seq],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Active members of a group other than `me`, with their delivered cursor, read cursor and
    /// whether they share read receipts: `(address, handle, delivered_seq, read_cursor, shares)`.
    pub async fn group_receipt_rows(
        &self,
        group_id: String,
        me: String,
    ) -> Result<Vec<GroupReceiptRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> Result<Vec<GroupReceiptRow>, StoreError> {
                let c = conn.lock().expect("store lock");
                let mut stmt = c.prepare(
                    "SELECT gm.member, i.handle, COALESCE(gr.delivered_seq, 0), gm.read_cursor,
                            COALESCE(rs.read_receipts, 1)
                       FROM group_members gm
                       LEFT JOIN group_receipts gr ON gr.group_id = gm.group_id AND gr.member = gm.member
                       LEFT JOIN receipt_settings rs ON rs.mailbox = gm.member
                       LEFT JOIN identities i ON i.address = gm.member
                      WHERE gm.group_id = ?1 AND gm.state = 'active' AND gm.member != ?2
                      ORDER BY gm.member",
                )?;
                let rows = stmt
                    .query_map(params![group_id, me], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get::<_, i64>(4)? != 0,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Move a message's send time back (tests of the edit window).
    #[cfg(test)]
    pub async fn backdate_message(&self, id: String, at: u64) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "UPDATE messages SET created_at = ?2 WHERE id = ?1",
                params![id, at as i64],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The `msg_ref` of one of `owner`'s rows, when it has one.
    pub async fn msg_ref_of(
        &self,
        owner: String,
        id: String,
    ) -> Result<Option<(String, Option<String>)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> Result<Option<(String, Option<String>)>, StoreError> {
                let c = conn.lock().expect("store lock");
                Ok(c.query_row(
                    "SELECT msg_ref, edit_of FROM message_meta WHERE owner = ?1 AND id = ?2",
                    params![owner, id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
            },
        )
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, owner: &str, out: bool) -> crate::store::Message {
        crate::store::Message {
            id: id.into(),
            recipient: "/k/b".into(),
            sender: "/k/a".into(),
            wrap_blob: vec![1],
            created_at: 100,
            read: out,
            owner: owner.into(),
            outgoing: out,
            thread_id: "t".into(),
            filtered: false,
        }
    }

    fn meta(id: &str, owner: &str, msg_ref: &str) -> MessageMeta {
        MessageMeta {
            id: id.into(),
            owner: owner.into(),
            msg_ref: msg_ref.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn receipts_only_move_forward_and_metadata_follows_its_row() {
        let store = Store::open(":memory:").unwrap();
        store
            .enqueue_copies(
                message("in1", "/k/b", false),
                message("out1", "/k/a", true),
                [
                    meta("in1", "/k/b", "mref_x"),
                    meta("out1", "/k/a", "mref_x"),
                ],
                None,
                None,
            )
            .await
            .unwrap();

        let seen = store
            .mark_received("/k/b".into(), vec!["in1".into()], ReceiptKind::Seen, 10)
            .await
            .unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            store
                .apply_receipt("/k/a".into(), "mref_x".into(), ReceiptKind::Seen, 10)
                .await
                .unwrap(),
            Some("out1".into())
        );
        // A later delivered changes nothing: seen implies delivered.
        assert_eq!(
            store
                .apply_receipt("/k/a".into(), "mref_x".into(), ReceiptKind::Delivered, 11)
                .await
                .unwrap(),
            None
        );
        let m = store
            .meta_for("/k/a".into(), vec!["out1".into()])
            .await
            .unwrap();
        assert_eq!(m["out1"].seen_at, Some(10));
        assert_eq!(m["out1"].delivered_at, Some(10));

        // The recipient's marker dedupes a second seen.
        assert!(store
            .mark_received("/k/b".into(), vec!["in1".into()], ReceiptKind::Seen, 12)
            .await
            .unwrap()
            .is_empty());

        assert!(store
            .delete_message("/k/a".into(), "out1".into())
            .await
            .unwrap());
        assert!(store
            .meta_for("/k/a".into(), vec!["out1".into()])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn metadata_failure_rolls_back_both_copies_and_expiry_is_atomic() {
        let store = Store::open(":memory:").unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_test_metadata BEFORE INSERT ON message_meta
             WHEN NEW.id = 'out1' BEGIN SELECT RAISE(ABORT, 'test metadata failure'); END;",
            )
            .unwrap();
        let write = || {
            store.enqueue_copies(
                message("in1", "/k/b", false),
                message("out1", "/k/a", true),
                [
                    meta("in1", "/k/b", "mref_x"),
                    meta("out1", "/k/a", "mref_x"),
                ],
                Some(u64::MAX / 4),
                None,
            )
        };
        assert!(write().await.is_err());
        assert!(store.list_for("/k/a".into()).await.unwrap().is_empty());
        assert!(store.list_for("/k/b".into()).await.unwrap().is_empty());
        assert!(store
            .meta_for("/k/b".into(), vec!["in1".into()])
            .await
            .unwrap()
            .is_empty());
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_test_metadata")
            .unwrap();
        assert!(matches!(write().await.unwrap(), DeliveryWrite::Stored));
        let expiry = store
            .message_expiries(vec!["in1".into(), "out1".into()])
            .await
            .unwrap();
        assert_eq!(expiry.len(), 2);
        assert_eq!(expiry["in1"], u64::MAX / 4);
        assert_eq!(expiry["out1"], expiry["in1"]);
    }

    #[tokio::test]
    async fn sending_to_self_keeps_both_directions_and_retries_return_the_originals() {
        let store = Store::open(":memory:").unwrap();
        let write = |incoming: &str, outgoing: &str| {
            let mut received = message(incoming, "/k/a", false);
            received.recipient = "/k/a".into();
            let mut sent = message(outgoing, "/k/a", true);
            sent.recipient = "/k/a".into();
            store.enqueue_copies(
                received,
                sent,
                [
                    meta(incoming, "/k/a", "mref_x"),
                    meta(outgoing, "/k/a", "mref_x"),
                ],
                None,
                None,
            )
        };
        assert!(matches!(
            write("in1", "out1").await.unwrap(),
            DeliveryWrite::Stored
        ));
        match write("in2", "out2").await.unwrap() {
            DeliveryWrite::Duplicate {
                message_id,
                sent_copy_id,
            } => {
                assert_eq!(message_id.as_deref(), Some("in1"));
                assert_eq!(sent_copy_id, "out1");
            }
            _ => panic!("retry must return the first self-delivery"),
        }
        assert_eq!(store.list_for("/k/a".into()).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_edit_rechecks_the_root_and_deadline_before_committing() {
        let store = Store::open(":memory:").unwrap();
        store
            .enqueue_copies(
                message("in1", "/k/b", false),
                message("out1", "/k/a", true),
                [
                    meta("in1", "/k/b", "mref_root"),
                    meta("out1", "/k/a", "mref_root"),
                ],
                None,
                None,
            )
            .await
            .unwrap();
        let edit = |deadline| {
            let mut received = meta("in2", "/k/b", "mref_edit");
            received.edit_of = Some("mref_root".into());
            let mut sent = meta("out2", "/k/a", "mref_edit");
            sent.edit_of = Some("mref_root".into());
            store.enqueue_copies(
                message("in2", "/k/b", false),
                message("out2", "/k/a", true),
                [received, sent],
                None,
                Some(deadline),
            )
        };
        assert!(matches!(
            edit(0).await.unwrap(),
            DeliveryWrite::EditWindowClosed
        ));
        store
            .delete_message("/k/a".into(), "out1".into())
            .await
            .unwrap();
        assert!(matches!(
            edit(u64::MAX).await.unwrap(),
            DeliveryWrite::ReferenceNotFound
        ));
        assert!(store.list_for("/k/a".into()).await.unwrap().is_empty());
        let recipient = store.list_for("/k/b".into()).await.unwrap();
        assert_eq!(recipient.len(), 1);
        assert_eq!(recipient[0].id, "in1");
    }

    #[tokio::test]
    async fn read_receipts_default_on_and_can_be_turned_off() {
        let store = Store::open(":memory:").unwrap();
        assert!(store.read_receipts("/k/a".into()).await.unwrap());
        store
            .set_read_receipts("/k/a".into(), false, 1)
            .await
            .unwrap();
        assert!(!store.read_receipts("/k/a".into()).await.unwrap());
    }
}
