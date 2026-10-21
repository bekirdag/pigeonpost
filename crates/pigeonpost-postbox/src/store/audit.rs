//! The append-only audit log (SDS §6.8).
//!
//! Append-only is enforced by the database, not by the absence of a method: triggers refuse every
//! `UPDATE`, and refuse a `DELETE` of any row still inside the two-year retention period. A future
//! retention sweep may remove expired rows; nothing can rewrite or remove a live one.

use super::{params, Connection, Store, StoreError};

/// Two years, the retention period in §6.8 (spelled out as 63072000 in the trigger).
#[cfg(test)]
pub(crate) const AUDIT_RETENTION_SECS: i64 = 2 * 365 * 86_400;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id        TEXT,
    at            INTEGER NOT NULL,
    actor_account TEXT,
    actor_kind    TEXT NOT NULL CHECK (actor_kind IN ('human','agent','api_key','system')),
    action        TEXT NOT NULL,
    target        TEXT,
    details       TEXT,
    ip_hash       TEXT,
    user_agent    TEXT
);
CREATE INDEX IF NOT EXISTS audit_log_by_org ON audit_log(org_id, at);
CREATE INDEX IF NOT EXISTS audit_log_by_actor ON audit_log(actor_account, at);
CREATE TRIGGER IF NOT EXISTS audit_log_no_update BEFORE UPDATE ON audit_log
BEGIN
    SELECT RAISE(ABORT, 'audit_log is append-only');
END;
CREATE TRIGGER IF NOT EXISTS audit_log_no_delete BEFORE DELETE ON audit_log
    WHEN OLD.at > CAST(strftime('%s','now') AS INTEGER) - 63072000
BEGIN
    SELECT RAISE(ABORT, 'audit_log is append-only');
END;
";

/// One row, already sanitised by `crate::audit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub org_id: Option<String>,
    pub at: u64,
    pub actor_account: Option<String>,
    pub actor_kind: &'static str,
    pub action: String,
    pub target: Option<String>,
    pub details: String,
    pub ip_hash: Option<String>,
    pub user_agent: Option<String>,
}

/// Append inside the caller's transaction, so the record commits or rolls back with the change it
/// describes.
pub(crate) fn append_in(c: &Connection, row: &AuditRow) -> Result<i64, StoreError> {
    c.execute(
        "INSERT INTO audit_log
            (org_id, at, actor_account, actor_kind, action, target, details, ip_hash, user_agent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.org_id,
            row.at as i64,
            row.actor_account,
            row.actor_kind,
            row.action,
            row.target,
            row.details,
            row.ip_hash,
            row.user_agent,
        ],
    )?;
    Ok(c.last_insert_rowid())
}

impl Store {
    /// Append one audit record on its own.
    pub async fn append_audit(&self, row: AuditRow) -> Result<i64, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<i64, StoreError> {
            let c = conn.lock().expect("store lock");
            append_in(&c, &row)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Records naming `actor_account`, newest first.
    #[cfg(test)]
    pub async fn audit_for_actor(
        &self,
        actor_account: Option<String>,
        limit: usize,
    ) -> Result<Vec<AuditRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<AuditRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT org_id, at, actor_account, actor_kind, action, target, details, ip_hash,
                        user_agent
                 FROM audit_log WHERE actor_account IS ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![actor_account, limit as i64], |r| {
                let kind: String = r.get(3)?;
                Ok(AuditRow {
                    org_id: r.get(0)?,
                    at: r.get::<_, i64>(1)? as u64,
                    actor_account: r.get(2)?,
                    actor_kind: match kind.as_str() {
                        "human" => "human",
                        "agent" => "agent",
                        "api_key" => "api_key",
                        _ => "system",
                    },
                    action: r.get(4)?,
                    target: r.get(5)?,
                    details: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    ip_hash: r.get(7)?,
                    user_agent: r.get(8)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Test hook: run raw SQL against the store, to prove what the schema refuses.
    #[cfg(test)]
    pub async fn exec_raw(&self, sql: &'static str) -> Result<usize, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.execute(sql, [])?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: u64) -> AuditRow {
        AuditRow {
            org_id: None,
            at,
            actor_account: Some("acct_1".into()),
            actor_kind: "human",
            action: "feature.set".into(),
            target: Some("account_events".into()),
            details: "{}".into(),
            ip_hash: None,
            user_agent: None,
        }
    }

    #[tokio::test]
    async fn audit_rows_cannot_be_updated_or_deleted() {
        let store = Store::open(":memory:").unwrap();
        let now = crate::now_unix();
        store.append_audit(row(now)).await.unwrap();

        let update = store
            .exec_raw("UPDATE audit_log SET action = 'forged'")
            .await
            .unwrap_err();
        assert!(update.to_string().contains("append-only"), "{update}");
        let delete = store.exec_raw("DELETE FROM audit_log").await.unwrap_err();
        assert!(delete.to_string().contains("append-only"), "{delete}");

        let rows = store
            .audit_for_actor(Some("acct_1".into()), 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "feature.set");
    }

    #[tokio::test]
    async fn only_rows_past_retention_may_be_removed() {
        let store = Store::open(":memory:").unwrap();
        let expired = crate::now_unix() - AUDIT_RETENTION_SECS as u64 - 86_400;
        store.append_audit(row(expired)).await.unwrap();
        store.append_audit(row(crate::now_unix())).await.unwrap();
        // The live row blocks a blanket delete…
        assert!(store.exec_raw("DELETE FROM audit_log").await.is_err());
        // …while a sweep of expired rows only is allowed.
        let removed = store
            .exec_raw(
                "DELETE FROM audit_log WHERE at < CAST(strftime('%s','now') AS INTEGER) - 63072000",
            )
            .await
            .unwrap();
        assert_eq!(removed, 1);
    }

    #[tokio::test]
    async fn actor_kind_is_constrained() {
        let store = Store::open(":memory:").unwrap();
        let mut bad = row(crate::now_unix());
        bad.actor_kind = "robot";
        assert!(store.append_audit(bad).await.is_err());
    }
}
