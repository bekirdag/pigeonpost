//! Server-side feature flags (SDS §22.5): global, per account, and (later) per org.
//!
//! An account-scoped value overrides the global one. A flag with no row anywhere is off. Changing
//! a flag commits its audit record — and, for an account flag, the account's `features.changed`
//! event — in the same transaction as the change.

use super::{audit, events, params, Store, StoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS features (
    name       TEXT NOT NULL,
    scope      TEXT NOT NULL CHECK (scope IN ('global','account','org')),
    scope_id   TEXT NOT NULL DEFAULT '',
    enabled    INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    updated_by TEXT,
    PRIMARY KEY (name, scope, scope_id)
);
";

/// Where a flag value applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureScope {
    Global,
    Account(String),
}

impl FeatureScope {
    fn parts(&self) -> (&'static str, &str) {
        match self {
            FeatureScope::Global => ("global", ""),
            FeatureScope::Account(id) => ("account", id.as_str()),
        }
    }
}

/// One stored flag value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeatureRow {
    pub name: String,
    pub scope: String,
    pub scope_id: String,
    pub enabled: bool,
    pub updated_at: u64,
    pub updated_by: Option<String>,
}

/// Whether one flag is on for `account` (or globally), for callers holding the lock.
pub(super) fn enabled_in(
    c: &rusqlite::Connection,
    name: &str,
    account: Option<&str>,
) -> Result<bool, StoreError> {
    use rusqlite::OptionalExtension;
    let enabled: Option<i64> = c
        .query_row(
            "SELECT enabled FROM features
             WHERE name = ?1 AND (
                 (scope = 'account' AND scope_id = ?2) OR (scope = 'global' AND scope_id = '')
             )
             ORDER BY CASE scope WHEN 'account' THEN 0 ELSE 1 END LIMIT 1",
            params![name, account.unwrap_or("")],
            |r| r.get(0),
        )
        .optional()?;
    Ok(enabled == Some(1))
}

impl Store {
    /// The flags enabled for `account` (or globally, for a caller with no account), among `known`.
    pub async fn enabled_features(
        &self,
        account: Option<String>,
        known: Vec<&'static str>,
    ) -> Result<Vec<&'static str>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<&'static str>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT enabled FROM features
                 WHERE name = ?1 AND (
                     (scope = 'account' AND scope_id = ?2) OR (scope = 'global' AND scope_id = '')
                 )
                 ORDER BY CASE scope WHEN 'account' THEN 0 ELSE 1 END LIMIT 1",
            )?;
            let mut on = Vec::new();
            for name in known {
                let enabled: Option<i64> = stmt
                    .query_row(params![name, account.as_deref().unwrap_or("")], |r| {
                        r.get(0)
                    })
                    .ok();
                if enabled == Some(1) {
                    on.push(name);
                }
            }
            Ok(on)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Every stored flag value.
    pub async fn list_features(&self) -> Result<Vec<FeatureRow>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<FeatureRow>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(
                "SELECT name, scope, scope_id, enabled, updated_at, updated_by FROM features
                 ORDER BY name, scope, scope_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FeatureRow {
                    name: r.get(0)?,
                    scope: r.get(1)?,
                    scope_id: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    updated_at: r.get::<_, i64>(4)? as u64,
                    updated_by: r.get(5)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Set (`Some`) or clear (`None`) one flag value, with its audit record and, for an account
    /// flag, the account's `features.changed` event — all in one transaction.
    pub async fn set_feature(
        &self,
        name: String,
        scope: FeatureScope,
        enabled: Option<bool>,
        updated_by: String,
        audit_row: audit::AuditRow,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let (scope_name, scope_id) = scope.parts();
            match enabled {
                Some(on) => {
                    tx.execute(
                        "INSERT INTO features (name, scope, scope_id, enabled, updated_at, updated_by)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(name, scope, scope_id) DO UPDATE SET
                             enabled = excluded.enabled, updated_at = excluded.updated_at,
                             updated_by = excluded.updated_by",
                        params![name, scope_name, scope_id, on as i64, now as i64, updated_by],
                    )?;
                }
                None => {
                    tx.execute(
                        "DELETE FROM features WHERE name = ?1 AND scope = ?2 AND scope_id = ?3",
                        params![name, scope_name, scope_id],
                    )?;
                }
            }
            audit::append_in(&tx, &audit_row)?;
            if let FeatureScope::Account(account) = &scope {
                events::append_in(
                    &tx,
                    account,
                    "features.changed",
                    None,
                    &serde_json::json!({ "feature": name, "enabled": enabled }),
                    now,
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit_row() -> audit::AuditRow {
        audit::AuditRow {
            org_id: None,
            at: 1,
            actor_account: None,
            actor_kind: "system",
            action: "feature.set".into(),
            target: Some("f".into()),
            details: "{}".into(),
            ip_hash: None,
            user_agent: None,
        }
    }

    #[tokio::test]
    async fn account_values_override_global_and_absent_means_off() {
        let store = Store::open(":memory:").unwrap();
        let known = vec!["alpha", "beta"];
        assert!(store
            .enabled_features(Some("a1".into()), known.clone())
            .await
            .unwrap()
            .is_empty());

        store
            .set_feature(
                "alpha".into(),
                FeatureScope::Global,
                Some(true),
                "ops".into(),
                audit_row(),
                1,
            )
            .await
            .unwrap();
        store
            .set_feature(
                "alpha".into(),
                FeatureScope::Account("a2".into()),
                Some(false),
                "ops".into(),
                audit_row(),
                2,
            )
            .await
            .unwrap();
        store
            .set_feature(
                "beta".into(),
                FeatureScope::Account("a1".into()),
                Some(true),
                "ops".into(),
                audit_row(),
                3,
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .enabled_features(Some("a1".into()), known.clone())
                .await
                .unwrap(),
            vec!["alpha", "beta"]
        );
        assert!(store
            .enabled_features(Some("a2".into()), known.clone())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store.enabled_features(None, known.clone()).await.unwrap(),
            vec!["alpha"]
        );

        // Clearing the override falls back to global.
        store
            .set_feature(
                "alpha".into(),
                FeatureScope::Account("a2".into()),
                None,
                "ops".into(),
                audit_row(),
                4,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .enabled_features(Some("a2".into()), known)
                .await
                .unwrap(),
            vec!["alpha"]
        );
        assert_eq!(store.list_features().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_flag_change_commits_its_audit_record_and_account_event_together() {
        let store = Store::open(":memory:").unwrap();
        store
            .set_feature(
                "alpha".into(),
                FeatureScope::Account("a1".into()),
                Some(true),
                "ops".into(),
                audit_row(),
                5,
            )
            .await
            .unwrap();
        let batch = store
            .stream_after(
                "a1".into(),
                vec![],
                events::StreamCursor::default(),
                true,
                10,
            )
            .await
            .unwrap();
        assert_eq!(batch.typed.len(), 1);
        assert_eq!(batch.typed[0].kind, "features.changed");
        assert_eq!(store.audit_for_actor(None, 10).await.unwrap().len(), 1);

        // A failing audit row (bad actor kind) rolls the flag back with it.
        let mut bad = audit_row();
        bad.actor_kind = "nobody";
        assert!(store
            .set_feature(
                "beta".into(),
                FeatureScope::Global,
                Some(true),
                "ops".into(),
                bad,
                6
            )
            .await
            .is_err());
        assert!(store
            .enabled_features(None, vec!["beta"])
            .await
            .unwrap()
            .is_empty());
    }
}
