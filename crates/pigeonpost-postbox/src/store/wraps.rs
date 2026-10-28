//! How message wraps are kept at rest (`messages.wrap_blob`, `message_meta.meta_blob`).
//!
//! Every stored wrap goes through [`encode_wrap`] and every read through [`decode_wrap`]. The
//! writer emits either the compact storage record from [`Wrap::to_storage_bytes`] (the default)
//! or the legacy JSON, chosen by `POSTBOX_WRAP_STORAGE`; the reader accepts both, so a database
//! may hold a mix. Nothing here is a wire format: wraps leave the server only as opened text.
//!
//! [`Store::rewrite_wraps`] converts existing rows between the two, which is what makes rolling
//! back to an image that only reads JSON safe. See `docs/ops/message-storage-encoding.md`.

use super::{params, Store, StoreError};
use pigeonpost_core::envelope::Wrap;
use rusqlite::{Connection, TransactionBehavior};
use std::sync::OnceLock;

/// Environment variable selecting the writer's format.
pub const WRAP_STORAGE_ENV: &str = "POSTBOX_WRAP_STORAGE";

/// Rows per transaction when rewriting.
pub const REWRITE_BATCH: usize = 500;

/// The at-rest format a writer emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapStorage {
    /// [`Wrap::to_storage_bytes`]: raw fields, raw ciphertext.
    Compact,
    /// `serde_json::to_vec(&wrap)`, what every server before this one wrote.
    Json,
}

impl WrapStorage {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Json => "json",
        }
    }

    /// The configured writer, or an error naming a value that is not one. Unset or empty means
    /// compact.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var(WRAP_STORAGE_ENV) {
            Ok(v) if v.trim().is_empty() => Ok(Self::Compact),
            Ok(v) => Self::parse(&v).ok_or_else(|| {
                format!("{WRAP_STORAGE_ENV} must be `compact` or `json`, not `{v}`")
            }),
            Err(std::env::VarError::NotPresent) => Ok(Self::Compact),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{WRAP_STORAGE_ENV} is not valid UTF-8"))
            }
        }
    }

    /// The writer this process uses, read once. The server refuses to start on an invalid value
    /// (see `main`), so the fallback here is only reached by code paths that never validated.
    pub fn configured() -> Self {
        static MODE: OnceLock<WrapStorage> = OnceLock::new();
        *MODE.get_or_init(|| Self::from_env().unwrap_or(Self::Compact))
    }

    /// Encode `wrap` in this format.
    pub fn encode(self, wrap: &Wrap) -> Result<Vec<u8>, StoreError> {
        match self {
            Self::Compact => wrap
                .to_storage_bytes()
                .map_err(|_| StoreError::Corrupt("wrap not encodable")),
            Self::Json => {
                serde_json::to_vec(wrap).map_err(|_| StoreError::Corrupt("wrap not encodable"))
            }
        }
    }

    /// Whether stored bytes are already in this format.
    fn holds(self, blob: &[u8]) -> bool {
        match self {
            Self::Compact => Wrap::is_storage_format(blob),
            Self::Json => !Wrap::is_storage_format(blob),
        }
    }
}

/// Encode a wrap for `messages.wrap_blob` / `message_meta.meta_blob` in the configured format.
pub fn encode_wrap(wrap: &Wrap) -> Result<Vec<u8>, StoreError> {
    WrapStorage::configured().encode(wrap)
}

/// Decode a stored wrap in either format.
pub fn decode_wrap(blob: &[u8]) -> Result<Wrap, StoreError> {
    Wrap::from_storage_bytes(blob).map_err(|_| StoreError::Corrupt("stored wrap"))
}

/// What a rewrite saw in one column.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RewriteTally {
    /// Non-null rows looked at.
    pub scanned: u64,
    /// Already in the target format; left alone.
    pub already: u64,
    /// Converted (or, on a dry run, convertible).
    pub converted: u64,
    /// Did not decode as a wrap in either format; left alone.
    pub undecodable: u64,
    /// Total stored bytes of the scanned rows before.
    pub bytes_before: u64,
    /// Total stored bytes of the scanned rows after (projected on a dry run).
    pub bytes_after: u64,
}

/// The result of [`Store::rewrite_wraps`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteReport {
    pub target: WrapStorage,
    pub applied: bool,
    pub messages: RewriteTally,
    pub meta: RewriteTally,
}

impl Store {
    /// Convert every stored wrap to `target`, in batches of `batch` rows per transaction.
    ///
    /// A dry run (`apply = false`) writes nothing and reports what would change. Each converted
    /// row is decoded again and compared with the original wrap before it is written, so a
    /// conversion can never change what a row says. Rows already in `target`, and rows that do not
    /// decode, are left as they are, which makes the operation idempotent and safe to resume.
    pub async fn rewrite_wraps(
        &self,
        target: WrapStorage,
        apply: bool,
        batch: usize,
    ) -> Result<RewriteReport, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<RewriteReport, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let messages = rewrite_column(&mut c, "messages", "wrap_blob", target, apply, batch)?;
            let meta = rewrite_column(&mut c, "message_meta", "meta_blob", target, apply, batch)?;
            Ok(RewriteReport {
                target,
                applied: apply,
                messages,
                meta,
            })
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

fn rewrite_column(
    c: &mut Connection,
    table: &'static str,
    column: &'static str,
    target: WrapStorage,
    apply: bool,
    batch: usize,
) -> Result<RewriteTally, StoreError> {
    let batch = batch.max(1);
    let mut tally = RewriteTally::default();
    let mut after_rowid = i64::MIN;
    loop {
        // IMMEDIATE takes the write lock up front, so a live server cannot change a row between
        // this batch's read and its write.
        let behavior = if apply {
            TransactionBehavior::Immediate
        } else {
            TransactionBehavior::Deferred
        };
        let tx = c.transaction_with_behavior(behavior)?;
        let rows: Vec<(i64, Vec<u8>)> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT rowid, {column} FROM {table}
                 WHERE rowid > ?1 AND {column} IS NOT NULL
                 ORDER BY rowid LIMIT ?2"
            ))?;
            let mapped = stmt.query_map(params![after_rowid, batch as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            mapped.collect::<Result<_, _>>()?
        };
        let Some(&(last, _)) = rows.last() else {
            break;
        };
        for (rowid, blob) in &rows {
            tally.scanned += 1;
            tally.bytes_before += blob.len() as u64;
            if target.holds(blob) {
                tally.already += 1;
                tally.bytes_after += blob.len() as u64;
                continue;
            }
            let Ok(wrap) = decode_wrap(blob) else {
                tally.undecodable += 1;
                tally.bytes_after += blob.len() as u64;
                continue;
            };
            let converted = target.encode(&wrap)?;
            // Verify before writing: the new bytes are in the target format and say exactly what
            // the old ones did.
            if !target.holds(&converted) || decode_wrap(&converted)? != wrap {
                return Err(StoreError::Corrupt(
                    "rewritten wrap does not match the original",
                ));
            }
            tally.converted += 1;
            tally.bytes_after += converted.len() as u64;
            if apply {
                tx.execute(
                    &format!("UPDATE {table} SET {column} = ?1 WHERE rowid = ?2 AND {column} = ?3"),
                    params![converted, rowid, blob],
                )?;
            }
        }
        tx.commit()?;
        after_rowid = last;
    }
    Ok(tally)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Message, MessageMeta};
    use pigeonpost_core::{envelope, Identity};

    fn pair() -> (Identity, Identity) {
        (Identity::from_seed([3; 32]), Identity::from_seed([4; 32]))
    }

    fn message(id: &str, blob: Vec<u8>) -> Message {
        Message {
            id: id.into(),
            recipient: "/k/bob".into(),
            sender: "/k/alice".into(),
            wrap_blob: blob,
            created_at: 1,
            read: false,
            owner: "/k/bob".into(),
            outgoing: false,
            thread_id: "t".into(),
            filtered: false,
        }
    }

    fn snapshot(c: &Connection) -> Vec<(String, Vec<u8>, Option<Vec<u8>>)> {
        let mut stmt = c
            .prepare(
                "SELECT m.id, m.wrap_blob, mm.meta_blob FROM messages m
                 LEFT JOIN message_meta mm ON mm.id = m.id ORDER BY m.rowid",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn writer_mode_parses_strictly() {
        assert_eq!(WrapStorage::parse("compact"), Some(WrapStorage::Compact));
        assert_eq!(WrapStorage::parse(" JSON "), Some(WrapStorage::Json));
        assert_eq!(WrapStorage::parse("binary"), None);
        assert_eq!(WrapStorage::parse(""), None);
    }

    #[test]
    fn both_formats_decode_to_the_same_wrap() {
        let (alice, bob) = pair();
        let wrap = envelope::wrap(&alice, &bob.verifying_key(), "hi", 1_786_000_000).unwrap();
        let compact = WrapStorage::Compact.encode(&wrap).unwrap();
        let json = WrapStorage::Json.encode(&wrap).unwrap();
        assert!(compact.len() < json.len());
        assert_eq!(decode_wrap(&compact).unwrap(), wrap);
        assert_eq!(decode_wrap(&json).unwrap(), wrap);
        assert!(decode_wrap(&[1, 2, 3]).is_err());
        assert!(decode_wrap(&[]).is_err());
    }

    /// json → compact → json leaves every row decoding to the same wrap, is idempotent, does not
    /// touch rows it cannot read, and shrinks what the quota counts.
    #[tokio::test]
    async fn rewrite_round_trips_and_is_idempotent() {
        let (alice, bob) = pair();
        let store = Store::open(":memory:").unwrap();
        let mut originals = Vec::new();
        for i in 0..7 {
            let body = "x".repeat(i * 300);
            let wrap = envelope::wrap(&alice, &bob.verifying_key(), &body, 1_786_000_000).unwrap();
            let meta = envelope::wrap(&alice, &bob.verifying_key(), "{}", 1_786_000_000).unwrap();
            let id = format!("m{i}");
            // A mix of formats, and some rows with no metadata envelope.
            let blob = if i % 3 == 0 {
                WrapStorage::Compact.encode(&wrap).unwrap()
            } else {
                WrapStorage::Json.encode(&wrap).unwrap()
            };
            let meta_blob = (i % 2 == 0).then(|| WrapStorage::Json.encode(&meta).unwrap());
            store
                .enqueue_copies(
                    message(&id, blob),
                    Message {
                        id: format!("{id}-sent"),
                        owner: "/k/alice".into(),
                        outgoing: true,
                        ..message(&id, WrapStorage::Json.encode(&wrap).unwrap())
                    },
                    [
                        MessageMeta {
                            id: id.clone(),
                            owner: "/k/bob".into(),
                            msg_ref: id.clone(),
                            meta_blob: meta_blob.clone(),
                            ..Default::default()
                        },
                        MessageMeta {
                            id: format!("{id}-sent"),
                            owner: "/k/alice".into(),
                            msg_ref: id.clone(),
                            meta_blob: None,
                            ..Default::default()
                        },
                    ],
                    None,
                    None,
                )
                .await
                .unwrap();
            originals.push((id, wrap, meta_blob.map(|_| meta)));
        }
        // A row that is not a wrap at all must survive untouched.
        store.enqueue(message("junk", vec![1, 2, 3])).await.unwrap();

        let decoded = |store: &Store| {
            let c = store.conn.lock().unwrap();
            snapshot(&c)
                .into_iter()
                .map(|(id, w, m)| {
                    (
                        id,
                        decode_wrap(&w).ok(),
                        m.map(|m| decode_wrap(&m).unwrap()),
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = decoded(&store);
        let quota_before = store.inbox_bytes("/k/bob".into()).await.unwrap();

        // Dry run: counts, but changes nothing.
        let raw_before = snapshot(&store.conn.lock().unwrap());
        let dry = store
            .rewrite_wraps(WrapStorage::Compact, false, 3)
            .await
            .unwrap();
        assert_eq!(snapshot(&store.conn.lock().unwrap()), raw_before);
        assert_eq!(dry.messages.scanned, 15);
        assert_eq!(dry.messages.undecodable, 1);
        assert_eq!(dry.messages.already, 3);
        assert_eq!(dry.messages.converted, 11);
        assert_eq!(dry.meta.scanned, 4);
        assert!(dry.messages.bytes_after < dry.messages.bytes_before);

        let applied = store
            .rewrite_wraps(WrapStorage::Compact, true, 3)
            .await
            .unwrap();
        assert_eq!(applied.messages, dry.messages);
        assert_eq!(applied.meta, dry.meta);
        assert_eq!(decoded(&store), before);
        assert!(store.inbox_bytes("/k/bob".into()).await.unwrap() < quota_before);
        for (_, w, m) in snapshot(&store.conn.lock().unwrap()) {
            if w != [1, 2, 3] {
                assert!(Wrap::is_storage_format(&w));
            }
            if let Some(m) = m {
                assert!(Wrap::is_storage_format(&m));
            }
        }

        // Again: nothing left to do.
        let again = store
            .rewrite_wraps(WrapStorage::Compact, true, 3)
            .await
            .unwrap();
        assert_eq!(again.messages.converted, 0);
        assert_eq!(again.meta.converted, 0);

        // And back, for a rollback.
        store
            .rewrite_wraps(WrapStorage::Json, true, REWRITE_BATCH)
            .await
            .unwrap();
        assert_eq!(decoded(&store), before);
        for (_, w, m) in snapshot(&store.conn.lock().unwrap()) {
            if w != [1, 2, 3] {
                let wrap: Wrap = serde_json::from_slice(&w).unwrap();
                assert_eq!(serde_json::to_vec(&wrap).unwrap(), w);
            }
            if let Some(m) = m {
                assert!(serde_json::from_slice::<Wrap>(&m).is_ok());
            }
        }
        for (id, wrap, meta) in originals {
            let row = before.iter().find(|(i, _, _)| *i == id).unwrap();
            assert_eq!(row.1.as_ref(), Some(&wrap));
            assert_eq!(row.2.as_ref(), meta.as_ref());
        }
    }
}
