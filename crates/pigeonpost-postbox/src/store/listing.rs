//! Bounded, index-backed mailbox listings for `GET /v1/inbox` (Phase 9b).
//!
//! The listing used to load the mailbox's whole history, every row with its sealed blob, and
//! filter it in Rust. A poll for "anything new?" against a mailbox of a few hundred messages read
//! all of them, and under load each abandoned request held a full history in memory (load test,
//! `docs/ops/2026-10-28-load-tests.md`).
//!
//! Now the store selects only the rows a request can show — the *page* — through an index:
//!
//! - an unread poll reads the unread rows (`messages_unread_by_mailbox`, a partial index that
//!   holds nothing but unread rows) and, when sent copies were asked for, the mailbox's sent rows;
//! - a history listing (`include_read`) walks `messages_by_mailbox` newest first and stops at the
//!   limit; a thread listing uses `messages_by_mailbox_thread`.
//!
//! What a page's rows *refer to* comes along as context, so the handler renders them exactly as it
//! did from the whole history: the originals and the other versions of edited rows, and the
//! message (and its edits) a reply quotes. Context is loaded, never listed, except the one case the
//! old listing also listed it — an original whose unread edit is on the page, shown folded.
//!
//! Rows still undelivered are found separately (`message_meta_undelivered`), because "a client
//! fetching the mailbox has received what is in it" (SDS §14C.6) covers rows a page may not show.

use super::{params, Message, OptionalExtension, Store, StoreError};
use std::collections::{HashMap, HashSet};

pub(super) const MIGRATIONS: &[&str] = &[
    // Unread rows only: what every poll asks for and `unread_count` counts. Both used to walk the
    // whole mailbox and read each row (blob included) to look at `read`.
    "CREATE INDEX IF NOT EXISTS messages_unread_by_mailbox
       ON messages(COALESCE(owner, recipient)) WHERE read = 0",
    // A thread's rows in one mailbox (`read_pigeonpost_thread`, CLI `thread`): `messages_by_thread`
    // alone also holds the other side's copies, and checking the mailbox needs the row.
    "CREATE INDEX IF NOT EXISTS messages_by_mailbox_thread
       ON messages(COALESCE(owner, recipient), thread_id)",
    // Rows that still owe their sender a delivered receipt. Small by construction: a listing
    // clears them.
    "CREATE INDEX IF NOT EXISTS message_meta_undelivered
       ON message_meta(owner) WHERE delivered_at IS NULL",
];

const NOT_EXPIRED: &str =
    "(expires_at IS NULL OR expires_at > CAST(strftime('%s','now') AS INTEGER))";

/// SQLite's bound-parameter limit is 999 on old builds; stay well under it.
const CHUNK: usize = 400;

/// Most context rows one page may pull in. A page refers to at most a few rows per row (its
/// original, its edits, its reply's parent and that parent's edits); this only stops a pathological
/// edit chain from turning one page back into a whole-history read.
const MAX_CONTEXT: usize = 20_000;

/// Most undelivered rows one listing marks delivered. Each listing clears what it marks, so a
/// backlog drains over a few listings rather than in one huge write.
const MAX_UNDELIVERED: usize = 5_000;

/// What a listing asks the store for.
#[derive(Debug, Clone)]
pub struct ListRequest {
    pub owner: String,
    /// History (`true`) or unread received mail only (`false`).
    pub include_read: bool,
    pub include_sent: bool,
    pub filtered_only: bool,
    pub thread_id: Option<String>,
    /// Only rows stored after this sequence number (a previous page's `newest_seq`), oldest first.
    pub after: Option<i64>,
    /// Only rows older than the row with this sequence number (a previous page's `oldest_seq`).
    pub before: Option<i64>,
    /// Most rows on the page.
    pub limit: usize,
    /// Tests: the old whole-history read, as the oracle the page is compared against.
    #[cfg(test)]
    pub legacy: bool,
}

/// A page of a mailbox and what its rows refer to.
#[derive(Default)]
pub struct Listing {
    /// Page and context rows, ordered as the full listing always was: by `created_at`, then by
    /// storage order.
    pub rows: Vec<Message>,
    /// Sequence number (rowid, never reused — `store::events`) of every row in `rows`.
    pub seq: HashMap<String, i64>,
    /// Ids of the rows on the page; everything else in `rows` is context.
    pub page: HashSet<String>,
    /// More rows match beyond this page, in the direction it was read.
    pub has_more: bool,
    /// Received rows in this mailbox still without a delivered receipt.
    pub undelivered: Vec<String>,
}

impl Listing {
    pub fn oldest_seq(&self) -> Option<i64> {
        self.page
            .iter()
            .filter_map(|id| self.seq.get(id))
            .min()
            .copied()
    }

    pub fn newest_seq(&self) -> Option<i64> {
        self.page
            .iter()
            .filter_map(|id| self.seq.get(id))
            .max()
            .copied()
    }
}

const ROW_COLUMNS: &str = "rowid, id, recipient, sender, wrap_blob, created_at, read,
    COALESCE(owner, recipient), direction, COALESCE(thread_id, ''), filtered";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Message)> {
    Ok((
        row.get(0)?,
        Message {
            id: row.get(1)?,
            recipient: row.get(2)?,
            sender: row.get(3)?,
            wrap_blob: row.get(4)?,
            created_at: row.get::<_, i64>(5)? as u64,
            read: row.get::<_, i64>(6)? != 0,
            owner: row.get(7)?,
            outgoing: row.get::<_, String>(8)? == "out",
            thread_id: row.get(9)?,
            filtered: row.get::<_, i64>(10)? != 0,
        },
    ))
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// `(rowid, created_at)` of the rows a page may be cut from, already in page order.
fn candidates(c: &rusqlite::Connection, req: &ListRequest) -> Result<(Vec<i64>, bool), StoreError> {
    let limit = req.limit.max(1);
    if req.include_read {
        // The direction/filing rules of the listing, in SQL, so a page counts only rows it shows.
        let shown = if req.filtered_only {
            "direction = 'in' AND filtered = 1"
        } else if req.include_sent {
            "(direction = 'out' OR filtered = 0)"
        } else {
            "direction = 'in' AND filtered = 0"
        };
        let thread = if req.thread_id.is_some() {
            "AND thread_id = ?2"
        } else {
            "AND ?2 IS NULL"
        };
        let (sql, cursor): (String, Option<i64>) = if let Some(after) = req.after {
            // Incremental: everything stored since the cursor, oldest first. The mailbox index
            // carries the rowid, so the cursor is checked without reading a row.
            (
                format!(
                    "SELECT rowid FROM messages INDEXED BY messages_by_mailbox
                      WHERE COALESCE(owner, recipient) = ?1 {thread} AND rowid > ?3
                        AND {shown} AND {NOT_EXPIRED}
                      ORDER BY rowid ASC LIMIT ?4"
                ),
                Some(after),
            )
        } else {
            // Newest first, stopping at the limit: the index is already in this order.
            // `before` names a row; its position is (created_at, rowid). A cursor whose row has
            // since been deleted still pages on, by storage order alone.
            let pivot: Option<i64> = match req.before {
                Some(before) => c
                    .query_row(
                        "SELECT created_at FROM messages WHERE rowid = ?1",
                        params![before],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let older = match (req.before, pivot) {
                (Some(_), Some(created)) => format!(
                    "AND (created_at < {created} OR (created_at = {created} AND rowid < ?3))"
                ),
                (Some(_), None) => "AND rowid < ?3".to_string(),
                (None, _) => "AND ?3 IS NULL".to_string(),
            };
            (
                format!(
                    "SELECT rowid FROM messages
                      WHERE COALESCE(owner, recipient) = ?1 {thread} {older}
                        AND {shown} AND {NOT_EXPIRED}
                      ORDER BY created_at DESC, rowid DESC LIMIT ?4"
                ),
                req.before,
            )
        };
        let mut stmt = c.prepare(&sql)?;
        let mut ids: Vec<i64> = stmt
            .query_map(
                params![req.owner, req.thread_id, cursor, (limit + 1) as i64],
                |r| r.get(0),
            )?
            .collect::<Result<_, _>>()?;
        let more = ids.len() > limit;
        ids.truncate(limit);
        return Ok((ids, more));
    }

    // Unread: the unread received rows (partial index), plus every sent copy when the caller asked
    // for both halves — the old listing's rule, kept.
    let mut rows: Vec<(i64, i64)> = Vec::new();
    {
        let mut stmt = c.prepare(&format!(
            "SELECT rowid, created_at FROM messages
              WHERE COALESCE(owner, recipient) = ?1 AND read = 0 AND direction = 'in'
                AND filtered = ?2 AND {NOT_EXPIRED}"
        ))?;
        for r in stmt.query_map(params![req.owner, req.filtered_only as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })? {
            rows.push(r?);
        }
    }
    if req.include_sent && !req.filtered_only {
        let mut stmt = c.prepare(&format!(
            "SELECT rowid, created_at FROM messages
              WHERE owner = ?1 AND direction = 'out' AND {NOT_EXPIRED}"
        ))?;
        for r in stmt.query_map(params![req.owner], |r| Ok((r.get(0)?, r.get(1)?)))? {
            rows.push(r?);
        }
    }
    rows.sort_by_key(|&(rowid, created)| (created, rowid));
    rows.dedup();
    if let Some(after) = req.after {
        rows.retain(|&(rowid, _)| rowid > after);
        rows.sort_by_key(|&(rowid, _)| rowid);
    }
    if let Some(before) = req.before {
        let pivot = rows
            .iter()
            .find(|&&(rowid, _)| rowid == before)
            .map(|&(_, created)| (created, before));
        let pivot = match pivot {
            Some(p) => Some(p),
            None => c
                .query_row(
                    "SELECT created_at FROM messages WHERE rowid = ?1",
                    params![before],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
                .map(|created| (created, before)),
        };
        match pivot {
            Some(p) => rows.retain(|&(rowid, created)| (created, rowid) < p),
            None => rows.clear(),
        }
    }
    // A queue to drain: oldest first, unless the caller is paging back from `before`.
    let more = rows.len() > limit;
    let page: Vec<i64> = if req.before.is_some() {
        rows[rows.len().saturating_sub(limit)..]
            .iter()
            .map(|&(rowid, _)| rowid)
            .collect()
    } else {
        rows.iter().take(limit).map(|&(rowid, _)| rowid).collect()
    };
    Ok((page, more))
}

fn rows_by_rowid(
    c: &rusqlite::Connection,
    owner: &str,
    rowids: &[i64],
) -> Result<Vec<(i64, Message)>, StoreError> {
    let mut out = Vec::with_capacity(rowids.len());
    for chunk in rowids.chunks(CHUNK) {
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM messages
              WHERE rowid IN ({}) AND COALESCE(owner, recipient) = ?",
            marks(chunk.len())
        );
        let mut stmt = c.prepare(&sql)?;
        let mut args: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|r| r as &dyn rusqlite::ToSql).collect();
        args.push(&owner);
        for row in stmt.query_map(args.as_slice(), read_row)? {
            out.push(row?);
        }
    }
    Ok(out)
}

fn rows_by_id(
    c: &rusqlite::Connection,
    owner: &str,
    ids: &[String],
) -> Result<Vec<(i64, Message)>, StoreError> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(CHUNK) {
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM messages
              WHERE id IN ({}) AND COALESCE(owner, recipient) = ? AND {NOT_EXPIRED}",
            marks(chunk.len())
        );
        let mut stmt = c.prepare(&sql)?;
        let mut args: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|r| r as &dyn rusqlite::ToSql).collect();
        args.push(&owner);
        for row in stmt.query_map(args.as_slice(), read_row)? {
            out.push(row?);
        }
    }
    Ok(out)
}

/// Ids of the rows that `seeds` refer to or that refer to them, by message reference: the
/// original and other versions of an edit, a reply's parent, and that parent's edits.
fn related_ids(
    c: &rusqlite::Connection,
    owner: &str,
    seeds: &[String],
) -> Result<HashSet<String>, StoreError> {
    let mut refs: HashSet<String> = HashSet::new();
    for chunk in seeds.chunks(CHUNK) {
        let sql = format!(
            "SELECT msg_ref, reply_to, edit_of FROM message_meta
              WHERE owner = ? AND id IN ({})",
            marks(chunk.len())
        );
        let mut stmt = c.prepare(&sql)?;
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&owner];
        args.extend(chunk.iter().map(|s| s as &dyn rusqlite::ToSql));
        for row in stmt.query_map(args.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (msg_ref, reply_to, edit_of) = row?;
            refs.insert(msg_ref);
            refs.extend(reply_to);
            refs.extend(edit_of);
        }
    }
    let refs: Vec<String> = refs.into_iter().collect();
    let mut out = HashSet::new();
    for chunk in refs.chunks(CHUNK) {
        let m = marks(chunk.len());
        // Two statements rather than an OR, so each uses its own index
        // (`message_meta_by_ref`, `message_meta_by_edit`).
        for sql in [
            format!("SELECT id FROM message_meta WHERE owner = ? AND msg_ref IN ({m})"),
            format!("SELECT id FROM message_meta WHERE owner = ? AND edit_of IN ({m})"),
        ] {
            let mut stmt = c.prepare(&sql)?;
            let mut args: Vec<&dyn rusqlite::ToSql> = vec![&owner];
            args.extend(chunk.iter().map(|s| s as &dyn rusqlite::ToSql));
            for id in stmt.query_map(args.as_slice(), |r| r.get::<_, String>(0))? {
                out.insert(id?);
            }
        }
    }
    Ok(out)
}

/// The pre-9b listing: every live row, all of it "the page". Tests compare against it.
#[cfg(test)]
fn legacy_in(c: &rusqlite::Connection, req: &ListRequest) -> Result<Listing, StoreError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {ROW_COLUMNS} FROM messages
          WHERE COALESCE(owner, recipient) = ?1 AND {NOT_EXPIRED}
          ORDER BY created_at ASC"
    ))?;
    let rows = stmt
        .query_map(params![req.owner], read_row)?
        .collect::<Result<Vec<_>, _>>()?;
    let mut undelivered = Vec::new();
    for (_, m) in rows.iter().filter(|(_, m)| !m.outgoing) {
        let pending: Option<bool> = c
            .query_row(
                "SELECT delivered_at IS NULL FROM message_meta WHERE owner = ?1 AND id = ?2",
                params![req.owner, m.id],
                |r| r.get(0),
            )
            .optional()?;
        if pending == Some(true) {
            undelivered.push(m.id.clone());
        }
    }
    Ok(Listing {
        seq: rows.iter().map(|(r, m)| (m.id.clone(), *r)).collect(),
        page: rows.iter().map(|(_, m)| m.id.clone()).collect(),
        rows: rows.into_iter().map(|(_, m)| m).collect(),
        has_more: false,
        undelivered,
    })
}

fn list_in(c: &rusqlite::Connection, req: &ListRequest) -> Result<Listing, StoreError> {
    #[cfg(test)]
    if req.legacy {
        return legacy_in(c, req);
    }
    let (page_rowids, has_more) = candidates(c, req)?;
    let mut rows = rows_by_rowid(c, &req.owner, &page_rowids)?;
    let page: HashSet<String> = rows.iter().map(|(_, m)| m.id.clone()).collect();
    let mut have: HashSet<String> = page.clone();

    // Two rounds: what the page refers to, then what *that* refers to — an original shown folded
    // because its edit is on the page still needs its own reply's parent.
    let mut seeds: Vec<String> = page.iter().cloned().collect();
    for _ in 0..2 {
        if seeds.is_empty() || have.len() - page.len() >= MAX_CONTEXT {
            break;
        }
        let mut new: Vec<String> = related_ids(c, &req.owner, &seeds)?
            .into_iter()
            .filter(|id| !have.contains(id))
            .collect();
        new.truncate(MAX_CONTEXT.saturating_sub(have.len() - page.len()));
        let loaded = rows_by_id(c, &req.owner, &new)?;
        seeds = loaded.iter().map(|(_, m)| m.id.clone()).collect();
        have.extend(seeds.iter().cloned());
        rows.extend(loaded);
    }
    rows.sort_by_key(|(rowid, m)| (m.created_at, *rowid));

    let undelivered = {
        let mut stmt = c.prepare(&format!(
            "SELECT m.id FROM message_meta mm JOIN messages m ON m.id = mm.id
              WHERE mm.owner = ?1 AND mm.delivered_at IS NULL AND m.direction = 'in'
                AND COALESCE(m.owner, m.recipient) = ?1
                AND (m.expires_at IS NULL OR m.expires_at > CAST(strftime('%s','now') AS INTEGER))
              LIMIT {MAX_UNDELIVERED}"
        ))?;
        let ids = stmt
            .query_map(params![req.owner], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };

    let seq = rows
        .iter()
        .map(|(rowid, m)| (m.id.clone(), *rowid))
        .collect();
    Ok(Listing {
        rows: rows.into_iter().map(|(_, m)| m).collect(),
        seq,
        page,
        has_more,
        undelivered,
    })
}

impl Store {
    /// One page of a mailbox for `GET /v1/inbox`, with the rows its rows refer to. See the module
    /// documentation.
    pub async fn list_page(&self, req: ListRequest) -> Result<Listing, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Listing, StoreError> {
            let c = conn.lock().expect("store lock");
            list_in(&c, &req)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// How many rows one thread holds in `owner`'s mailbox (expired rows excluded).
    pub async fn thread_message_count(
        &self,
        owner: String,
        thread_id: String,
    ) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            let n: i64 = c.query_row(
                &format!(
                    "SELECT COUNT(*) FROM messages
                      WHERE COALESCE(owner, recipient) = ?1 AND thread_id = ?2 AND {NOT_EXPIRED}"
                ),
                params![owner, thread_id],
                |r| r.get(0),
            )?;
            Ok(n as usize)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The query plan SQLite chose for a listing's first step, for the index tests.
    #[cfg(test)]
    pub async fn explain_unread(&self, owner: String) -> Result<String, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<String, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut out = Vec::new();
            for sql in [
                "EXPLAIN QUERY PLAN SELECT rowid, created_at FROM messages
                  WHERE COALESCE(owner, recipient) = ?1 AND read = 0 AND direction = 'in'
                    AND filtered = 0",
                "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM messages
                  WHERE COALESCE(owner, recipient) = ?1 AND read = 0 AND direction = 'in'
                    AND filtered = 0",
            ] {
                let mut stmt = c.prepare(sql)?;
                for row in stmt.query_map(params![owner], |r| r.get::<_, String>(3))? {
                    out.push(row?);
                }
            }
            Ok(out.join("\n"))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}
