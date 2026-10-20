//! Profile pictures (SDS §16.2): which picture a mailbox shows, who may see it, and the blobs it is
//! made of. The bytes live in the blob store under its own `avatars/` root; this is everything
//! else.

use super::{params, Connection, OptionalExtension, Store, StoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS avatars (
    id             TEXT PRIMARY KEY,
    owner_kind     TEXT NOT NULL CHECK (owner_kind IN ('identity','group','org')),
    owner_id       TEXT NOT NULL,
    sha256         BLOB NOT NULL,
    variants       TEXT NOT NULL,
    visibility     TEXT NOT NULL DEFAULT 'known' CHECK (visibility IN ('everyone','known','nobody')),
    version        INTEGER NOT NULL DEFAULT 1,
    created_at     INTEGER NOT NULL,
    removed_at     INTEGER,
    removed_reason TEXT
);
CREATE INDEX IF NOT EXISTS avatars_by_owner ON avatars(owner_kind, owner_id, created_at);
CREATE TABLE IF NOT EXISTS avatar_reports (
    avatar_id  TEXT NOT NULL,
    reporter   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (avatar_id, reporter)
);
";

pub(super) const MIGRATIONS: &[&str] = &["ALTER TABLE identities ADD COLUMN avatar_id TEXT"];

/// Changes allowed per mailbox per day (SDS §16.3). Uploads count; visibility changes and
/// removals do not.
pub const CHANGES_PER_DAY: usize = 5;

/// Who may see a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Everyone,
    Known,
    Nobody,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Everyone => "everyone",
            Visibility::Known => "known",
            Visibility::Nobody => "nobody",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "everyone" => Visibility::Everyone,
            "known" => Visibility::Known,
            "nobody" => Visibility::Nobody,
            _ => return None,
        })
    }
}

/// One stored picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarRow {
    pub id: String,
    /// The mailbox it belongs to.
    pub owner: String,
    pub sha256: String,
    /// Variant name (`64`, `128`, `256`, `512`, `jpeg`) → blob digest.
    pub variants: std::collections::BTreeMap<String, String>,
    pub visibility: Visibility,
    pub version: i64,
    pub created_at: u64,
}

/// A picture ready to store.
pub struct NewAvatar {
    pub id: String,
    pub sha256: String,
    pub variants: std::collections::BTreeMap<String, String>,
    pub visibility: Visibility,
}

/// What storing a new picture did.
#[derive(Debug)]
pub enum SetAvatar {
    Stored {
        row: AvatarRow,
        /// Blob digests nothing refers to any more.
        orphaned: Vec<String>,
    },
    RateLimited,
}

const COLS: &str = "a.id, a.owner_id, a.sha256, a.variants, a.visibility, a.version, a.created_at";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AvatarRow> {
    Ok(AvatarRow {
        id: r.get(0)?,
        owner: r.get(1)?,
        sha256: String::from_utf8_lossy(&r.get::<_, Vec<u8>>(2)?).into_owned(),
        variants: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
        visibility: Visibility::parse(&r.get::<_, String>(4)?).unwrap_or(Visibility::Nobody),
        version: r.get(5)?,
        created_at: r.get::<_, i64>(6)? as u64,
    })
}

/// The live picture of a mailbox, for callers holding the lock.
fn current_in(c: &Connection, address: &str) -> Result<Option<AvatarRow>, StoreError> {
    Ok(c.query_row(
        &format!(
            "SELECT {COLS} FROM identities i JOIN avatars a ON a.id = i.avatar_id
              WHERE i.address = ?1 AND a.removed_at IS NULL"
        ),
        params![address],
        read_row,
    )
    .optional()?)
}

/// Retire a picture and return the blob digests no live picture still uses.
fn retire_in(
    c: &Connection,
    address: &str,
    reason: &str,
    now: u64,
) -> Result<Vec<String>, StoreError> {
    let Some(old) = current_in(c, address)? else {
        c.execute(
            "UPDATE identities SET avatar_id = NULL WHERE address = ?1",
            params![address],
        )?;
        return Ok(Vec::new());
    };
    c.execute(
        "UPDATE avatars SET removed_at = ?2, removed_reason = ?3 WHERE id = ?1",
        params![old.id, now as i64, reason],
    )?;
    c.execute(
        "UPDATE identities SET avatar_id = NULL WHERE address = ?1",
        params![address],
    )?;
    let mut orphaned = Vec::new();
    for digest in old.variants.values() {
        let still: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM avatars
                             WHERE removed_at IS NULL AND instr(variants, ?1) > 0)",
            params![digest],
            |r| r.get::<_, i64>(0).map(|n| n != 0),
        )?;
        if !still && !orphaned.contains(digest) {
            orphaned.push(digest.clone());
        }
    }
    Ok(orphaned)
}

impl Store {
    /// Store a new picture for `address`, replacing the current one, unless the mailbox has used
    /// its daily changes.
    pub async fn set_avatar(
        &self,
        address: String,
        new: NewAvatar,
        now: u64,
    ) -> Result<SetAvatar, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<SetAvatar, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let recent: i64 = tx.query_row(
                "SELECT COUNT(*) FROM avatars
                  WHERE owner_kind = 'identity' AND owner_id = ?1 AND created_at > ?2",
                params![address, now.saturating_sub(86_400) as i64],
                |r| r.get(0),
            )?;
            if recent as usize >= CHANGES_PER_DAY {
                return Ok(SetAvatar::RateLimited);
            }
            let mut orphaned = retire_in(&tx, &address, "replaced", now)?;
            tx.execute(
                "INSERT INTO avatars (id, owner_kind, owner_id, sha256, variants, visibility,
                                      version, created_at)
                 VALUES (?1, 'identity', ?2, ?3, ?4, ?5, 1, ?6)",
                params![
                    new.id,
                    address,
                    new.sha256.as_bytes(),
                    serde_json::to_string(&new.variants).expect("map serializes"),
                    new.visibility.as_str(),
                    now as i64,
                ],
            )?;
            tx.execute(
                "UPDATE identities SET avatar_id = ?2 WHERE address = ?1",
                params![address, new.id],
            )?;
            // A digest the new picture reuses is not an orphan.
            orphaned.retain(|d| !new.variants.values().any(|v| v == d));
            let row = current_in(&tx, &address)?.ok_or(StoreError::Corrupt("avatar"))?;
            tx.commit()?;
            Ok(SetAvatar::Stored { row, orphaned })
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The live picture of a mailbox, if any.
    pub async fn avatar_of(&self, address: String) -> Result<Option<AvatarRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<AvatarRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            current_in(&c, &address)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// A live picture by id (the authorization resource behind every blob URL).
    pub async fn avatar_by_id(&self, id: String) -> Result<Option<AvatarRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<AvatarRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                &format!(
                    "SELECT {COLS} FROM avatars a JOIN identities i
                        ON i.address = a.owner_id AND i.avatar_id = a.id
                      WHERE a.id = ?1 AND a.removed_at IS NULL"
                ),
                params![id],
                read_row,
            )
            .optional()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Change who may see the live picture. Bumps the version, so every earlier URL stops working.
    pub async fn set_avatar_visibility(
        &self,
        address: String,
        visibility: Visibility,
    ) -> Result<Option<AvatarRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<AvatarRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            let Some(row) = current_in(&c, &address)? else {
                return Ok(None);
            };
            c.execute(
                "UPDATE avatars SET visibility = ?2, version = version + 1 WHERE id = ?1",
                params![row.id, visibility.as_str()],
            )?;
            current_in(&c, &address)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Remove the live picture. Returns the blob digests to delete.
    pub async fn remove_avatar(
        &self,
        address: String,
        reason: &'static str,
        now: u64,
    ) -> Result<(bool, Vec<String>), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(bool, Vec<String>), StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let had = current_in(&tx, &address)?.is_some();
            let orphaned = retire_in(&tx, &address, reason, now)?;
            tx.commit()?;
            Ok((had, orphaned))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Record a report. `Some(false)` when this reporter already reported this picture, `None`
    /// when the reporter has used its daily allowance.
    pub async fn report_avatar(
        &self,
        avatar_id: String,
        reporter: String,
        now: u64,
        per_day: usize,
    ) -> Result<Option<bool>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<bool>, StoreError> {
            let c = conn.lock().expect("store lock");
            let today: i64 = c.query_row(
                "SELECT COUNT(*) FROM avatar_reports WHERE reporter = ?1 AND created_at > ?2",
                params![reporter, now.saturating_sub(86_400) as i64],
                |r| r.get(0),
            )?;
            if today as usize >= per_day {
                return Ok(None);
            }
            Ok(Some(
                c.execute(
                    "INSERT OR IGNORE INTO avatar_reports (avatar_id, reporter, created_at)
                     VALUES (?1, ?2, ?3)",
                    params![avatar_id, reporter, now as i64],
                )? > 0,
            ))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Live pictures with open reports, most reported first, for the operator.
    pub async fn reported_avatars(&self) -> Result<Vec<(AvatarRow, u64)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<(AvatarRow, u64)>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(&format!(
                "SELECT {COLS}, COUNT(r.reporter) FROM avatars a
                   JOIN avatar_reports r ON r.avatar_id = a.id
                  WHERE a.removed_at IS NULL
                  GROUP BY a.id ORDER BY COUNT(r.reporter) DESC, a.created_at DESC LIMIT 200"
            ))?;
            let rows = stmt
                .query_map([], |r| Ok((read_row(r)?, r.get::<_, i64>(7)? as u64)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The mailboxes that should hear about `address`'s picture changing: its exact contacts and
    /// everyone it has corresponded with, hosted here. `(address, handle, account)`.
    #[allow(clippy::type_complexity)]
    pub async fn avatar_audience(
        &self,
        address: String,
    ) -> Result<Vec<(String, Option<String>, String)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> Result<Vec<(String, Option<String>, String)>, StoreError> {
                let c = conn.lock().expect("store lock");
                let mut stmt = c.prepare(
                    "SELECT i.address, i.handle, i.account_id FROM identities i
                      WHERE i.account_id IS NOT NULL AND i.address != ?1 AND (
                            i.address IN (SELECT peer FROM contacts
                                           WHERE owner = ?1 AND admission = 'allow')
                         OR i.handle IN (SELECT peer FROM contacts
                                          WHERE owner = ?1 AND admission = 'allow')
                         OR i.address IN (SELECT DISTINCT recipient FROM messages
                                           WHERE owner = ?1 AND direction = 'out')
                         OR i.address IN (SELECT DISTINCT sender FROM messages
                                           WHERE owner = ?1 AND direction = 'in'
                                             AND filtered = 0))
                      LIMIT 500",
                )?;
                let rows = stmt
                    .query_map(params![address], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoredIdentity;
    use std::collections::BTreeMap;

    fn new(id: &str, digest: &str) -> NewAvatar {
        let mut variants = BTreeMap::new();
        for v in ["64", "128", "256", "512", "jpeg"] {
            variants.insert(v.to_string(), format!("{digest}-{v}"));
        }
        NewAvatar {
            id: id.into(),
            sha256: digest.into(),
            variants,
            visibility: Visibility::Known,
        }
    }

    #[tokio::test]
    async fn replacing_retires_the_old_picture_and_frees_its_blobs() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(StoredIdentity::test_fixture("/k/a"))
            .await
            .unwrap();
        let SetAvatar::Stored { row, orphaned } = store
            .set_avatar("/k/a".into(), new("av1", "d1"), 10)
            .await
            .unwrap()
        else {
            panic!("stored")
        };
        assert!(orphaned.is_empty());
        assert_eq!(row.version, 1);
        let SetAvatar::Stored { orphaned, .. } = store
            .set_avatar("/k/a".into(), new("av2", "d2"), 11)
            .await
            .unwrap()
        else {
            panic!("stored")
        };
        assert_eq!(orphaned.len(), 5);
        assert!(store.avatar_by_id("av1".into()).await.unwrap().is_none());
        assert_eq!(
            store.avatar_of("/k/a".into()).await.unwrap().unwrap().id,
            "av2"
        );
        // Visibility bumps the version.
        let row = store
            .set_avatar_visibility("/k/a".into(), Visibility::Everyone)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((row.version, row.visibility), (2, Visibility::Everyone));
        let (had, orphaned) = store
            .remove_avatar("/k/a".into(), "removed", 12)
            .await
            .unwrap();
        assert!(had && orphaned.len() == 5);
        assert!(store.avatar_of("/k/a".into()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn five_changes_a_day() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert(StoredIdentity::test_fixture("/k/a"))
            .await
            .unwrap();
        for i in 0..CHANGES_PER_DAY {
            assert!(matches!(
                store
                    .set_avatar(
                        "/k/a".into(),
                        new(&format!("x{i}"), &format!("d{i}")),
                        100 + i as u64
                    )
                    .await
                    .unwrap(),
                SetAvatar::Stored { .. }
            ));
        }
        assert!(matches!(
            store
                .set_avatar("/k/a".into(), new("x9", "d9"), 200)
                .await
                .unwrap(),
            SetAvatar::RateLimited
        ));
        // A day later it opens again.
        assert!(matches!(
            store
                .set_avatar("/k/a".into(), new("y1", "e1"), 100 + 86_401 + 10)
                .await
                .unwrap(),
            SetAvatar::Stored { .. }
        ));
    }
}
