use super::{params, Connection, OptionalExtension, Store, StoreError};
use crate::googleplay::{VerifiedPurchase, MAX_HANDLES};
use serde::Serialize;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS google_subscriptions (
    purchase_token TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    product_id TEXT NOT NULL,
    namespace TEXT,
    expires_at INTEGER NOT NULL,
    state TEXT NOT NULL,
    auto_renewing INTEGER NOT NULL,
    acknowledged INTEGER NOT NULL,
    test_purchase INTEGER NOT NULL,
    verified_at INTEGER NOT NULL,
    replaced_by TEXT
);
CREATE INDEX IF NOT EXISTS google_subscriptions_by_account ON google_subscriptions(account_id);
CREATE INDEX IF NOT EXISTS google_subscriptions_by_verification ON google_subscriptions(verified_at);
";

#[derive(Clone, Serialize)]
pub struct GoogleSubscription {
    #[serde(skip)]
    pub purchase_token: String,
    #[serde(skip)]
    pub account_id: String,
    pub product_id: String,
    pub namespace: Option<String>,
    pub expires_at: u64,
    pub state: String,
    pub active: bool,
    pub auto_renewing: bool,
    pub acknowledged: bool,
    pub test_purchase: bool,
    #[serde(skip)]
    replaced_by: Option<String>,
}

pub enum GoogleBinding {
    Saved(GoogleSubscription),
    AccountMismatch,
    DuplicateProduct,
    LimitReached,
    Inactive,
    Replaced,
}

fn row(row: &rusqlite::Row<'_>, now: u64) -> rusqlite::Result<GoogleSubscription> {
    let expires_at = row.get::<_, i64>(4)?.max(0) as u64;
    let state: String = row.get(5)?;
    let replaced_by: Option<String> = row.get(9)?;
    let active = expires_at > now
        && replaced_by.is_none()
        && matches!(
            state.as_str(),
            "SUBSCRIPTION_STATE_ACTIVE"
                | "SUBSCRIPTION_STATE_CANCELED"
                | "SUBSCRIPTION_STATE_IN_GRACE_PERIOD"
        );
    Ok(GoogleSubscription {
        purchase_token: row.get(0)?,
        account_id: row.get(1)?,
        product_id: row.get(2)?,
        namespace: row.get(3)?,
        expires_at,
        state,
        active,
        auto_renewing: row.get(6)?,
        acknowledged: row.get(7)?,
        test_purchase: row.get(8)?,
        replaced_by,
    })
}
const COLUMNS: &str = "purchase_token, account_id, product_id, namespace, expires_at, state, auto_renewing, acknowledged, test_purchase, replaced_by";
fn lookup(c: &Connection, token: &str, now: u64) -> Result<Option<GoogleSubscription>, StoreError> {
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM google_subscriptions WHERE purchase_token=?1"),
        [token],
        |r| row(r, now),
    )
    .optional()?)
}

impl Store {
    pub async fn google_subscriptions_for(
        &self,
        account: String,
        now: u64,
    ) -> Result<Vec<GoogleSubscription>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM google_subscriptions WHERE account_id=?1 AND replaced_by IS NULL ORDER BY product_id, expires_at DESC"))?;
            let rows = stmt.query_map([account], |r| row(r, now))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await.map_err(|_| StoreError::Join)?
    }

    /// The paid slot is durable even when its requested name was taken during checkout. Its owner
    /// can assign another available name without another payment. Tokens never change account.
    pub async fn record_google_purchase(
        &self,
        account: String,
        token: String,
        purchase: VerifiedPurchase,
        requested_name: Option<String>,
        now: u64,
    ) -> Result<GoogleBinding, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<GoogleBinding, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let prior = lookup(&tx, &token, now)?;
            let linked = purchase.linked_token.as_deref().filter(|t| *t != token)
                .map(|t| lookup(&tx, t, now)).transpose()?.flatten();
            for old in prior.iter().chain(linked.iter()) {
                if old.account_id != account || old.product_id != purchase.product_id { return Ok(GoogleBinding::AccountMismatch); }
                if old.replaced_by.as_deref().is_some_and(|t| t != token) { return Ok(GoogleBinding::Replaced); }
            }
            if !purchase.active && prior.is_none() { return Ok(GoogleBinding::Inactive); }
            if purchase.active {
                let excluded = linked.as_ref().map(|p| p.purchase_token.as_str()).unwrap_or("");
                let live = "account_id=?1 AND purchase_token!=?2 AND purchase_token!=?3 AND replaced_by IS NULL AND expires_at>?4 AND state IN ('SUBSCRIPTION_STATE_ACTIVE','SUBSCRIPTION_STATE_CANCELED','SUBSCRIPTION_STATE_IN_GRACE_PERIOD')";
                let count: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM google_subscriptions WHERE {live}"),
                    params![account, token, excluded, now as i64], |r| r.get(0))?;
                if count >= MAX_HANDLES as i64 { return Ok(GoogleBinding::LimitReached); }
                let duplicate: bool = tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM google_subscriptions WHERE {live} AND product_id=?5)"),
                    params![account, token, excluded, now as i64, purchase.product_id], |r| r.get(0))?;
                if duplicate { return Ok(GoogleBinding::DuplicateProduct); }
            }
            let old_name = prior.as_ref().and_then(|s| s.namespace.clone())
                .or_else(|| linked.as_ref().and_then(|s| s.namespace.clone()));
            let mut namespace = old_name.clone().or(requested_name);
            if purchase.active {
                if let Some(name) = &namespace {
                    let occupied = !super::lifecycle::available_to(&tx, name, Some(&account), now)?;
                    let other_binding: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM namespaces WHERE namespace=?1
                        AND account_id=?2 AND (expires_at IS NULL OR expires_at>?3)
                        AND (source!='google' OR ?4 IS NULL OR ?4!=namespace))",
                        params![name, account, now as i64, old_name], |r| r.get(0))?;
                    let other_purchase: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM google_subscriptions WHERE namespace=?1 AND purchase_token!=?2 AND purchase_token!=?3 AND replaced_by IS NULL AND expires_at>?4)",
                        params![name, token, purchase.linked_token.as_deref().unwrap_or(""), now as i64], |r| r.get(0))?;
                    if occupied || other_binding || other_purchase { namespace = None; }
                }
            }
            let expiry = if purchase.active { purchase.expires_at } else { purchase.expires_at.min(now) };
            tx.execute("INSERT INTO google_subscriptions (purchase_token,account_id,product_id,namespace,expires_at,state,auto_renewing,acknowledged,test_purchase,verified_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                ON CONFLICT(purchase_token) DO UPDATE SET namespace=excluded.namespace,expires_at=excluded.expires_at,state=excluded.state,
                auto_renewing=excluded.auto_renewing,acknowledged=excluded.acknowledged,test_purchase=excluded.test_purchase,verified_at=excluded.verified_at",
                params![token, account, purchase.product_id, namespace, expiry as i64, purchase.state,
                    purchase.auto_renewing, purchase.acknowledged, purchase.test_purchase, now as i64])?;
            if purchase.active {
                if let Some(linked) = linked {
                    tx.execute("UPDATE google_subscriptions SET replaced_by=?1,expires_at=MIN(expires_at,?2) WHERE purchase_token=?3",
                        params![token, now as i64, linked.purchase_token])?;
                }
                if let Some(name) = &namespace {
                    super::lifecycle::detach_previous_aliases(&tx, name, &account)?;
                    tx.execute("INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at,provider_ref) VALUES (?1,?2,'google',?3,?4,?5)
                        ON CONFLICT(namespace) DO UPDATE SET account_id=excluded.account_id,source=excluded.source,verified_at=excluded.verified_at,expires_at=excluded.expires_at,provider_ref=?5,release_at=NULL",
                        params![name, account, now as i64, expiry as i64, token])?;
                }
            } else if let Some(name) = &old_name {
                // A stale token must never revoke a namespace backed by its replacement.
                let another: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM google_subscriptions WHERE namespace=?1 AND account_id=?2 AND purchase_token!=?3 AND replaced_by IS NULL AND expires_at>?4)",
                    params![name, account, token, now as i64], |r| r.get(0))?;
                if !another {
                    let terminal = purchase.state == "SUBSCRIPTION_STATE_EXPIRED";
                    tx.execute("UPDATE namespaces SET expires_at=MIN(expires_at,?1),verified_at=?2,
                        release_at=CASE WHEN ?3 THEN MIN(expires_at,?1)+?4 ELSE NULL END
                        WHERE namespace=?5 AND account_id=?6 AND source='google' AND provider_ref=?7",
                        params![expiry as i64, now as i64, terminal, super::lifecycle::RECOVERY_SECONDS, name, account, token])?;
                }
            }
            let saved = lookup(&tx, &token, now)?.ok_or(StoreError::Corrupt("Google purchase disappeared"))?;
            tx.commit()?;
            Ok(GoogleBinding::Saved(saved))
        }).await.map_err(|_| StoreError::Join)?
    }

    pub async fn acknowledge_google_purchase(&self, token: String) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            conn.lock().expect("store lock").execute(
                "UPDATE google_subscriptions SET acknowledged=1 WHERE purchase_token=?1",
                [token],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn google_due(&self, now: u64) -> Result<Vec<GoogleSubscription>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM google_subscriptions WHERE replaced_by IS NULL AND verified_at<?1 AND (expires_at>?2 OR EXISTS(SELECT 1 FROM namespaces n WHERE n.source='google' AND n.provider_ref=google_subscriptions.purchase_token)) ORDER BY verified_at LIMIT 100"))?;
            let rows = stmt.query_map(params![now.saturating_sub(300) as i64, now.saturating_sub(60 * 86400) as i64], |r| row(r, now))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await.map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purchase(slot: usize, expiry: u64) -> VerifiedPurchase {
        VerifiedPurchase {
            product_id: format!("pigeonpost.handle.{slot:02}"),
            linked_token: None,
            expires_at: expiry,
            state: "SUBSCRIPTION_STATE_ACTIVE".into(),
            active: true,
            auto_renewing: true,
            acknowledged: false,
            test_purchase: true,
        }
    }
    fn saved(result: GoogleBinding) -> GoogleSubscription {
        match result {
            GoogleBinding::Saved(row) => row,
            _ => panic!("purchase was refused"),
        }
    }
    async fn bind(
        store: &Store,
        account: &str,
        token: &str,
        slot: usize,
        name: &str,
    ) -> GoogleBinding {
        store
            .record_google_purchase(
                account.into(),
                token.into(),
                purchase(slot, 500),
                Some(name.into()),
                100,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn google_name_race_preserves_a_paid_slot_for_another_name() {
        let store = Store::open(":memory:").unwrap();
        let (a, b) = tokio::join!(
            bind(&store, "a", "token-a", 1, "alex"),
            bind(&store, "b", "token-b", 1, "alex")
        );
        let a = saved(a);
        let b = saved(b);
        assert_eq!(
            usize::from(a.namespace.is_some()) + usize::from(b.namespace.is_some()),
            1
        );
        let waiting = if a.namespace.is_none() { a } else { b };
        assert!(waiting.active);
        store
            .acknowledge_google_purchase(waiting.purchase_token.clone())
            .await
            .unwrap();
        assert!(
            store
                .google_subscriptions_for(waiting.account_id.clone(), 100)
                .await
                .unwrap()[0]
                .acknowledged
        );
        let assigned = saved(
            bind(
                &store,
                &waiting.account_id,
                &waiting.purchase_token,
                1,
                "alex-new",
            )
            .await,
        );
        assert_eq!(assigned.namespace.as_deref(), Some("alex-new"));
        assert_eq!(
            store
                .google_subscriptions_for(waiting.account_id, 100)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn google_token_cannot_move_accounts_or_buy_the_same_slot_twice() {
        let store = Store::open(":memory:").unwrap();
        saved(bind(&store, "a", "one", 1, "alex").await);
        assert!(matches!(
            bind(&store, "b", "one", 1, "other").await,
            GoogleBinding::AccountMismatch
        ));
        assert!(matches!(
            bind(&store, "a", "two", 1, "other").await,
            GoogleBinding::DuplicateProduct
        ));
        let restored = saved(bind(&store, "a", "one", 1, "renamed").await);
        assert_eq!(restored.namespace.as_deref(), Some("alex"));
    }

    #[tokio::test]
    async fn google_limit_is_atomic_and_restore_does_not_consume_a_slot() {
        let store = Store::open(":memory:").unwrap();
        for slot in 1..10 {
            saved(
                bind(
                    &store,
                    "a",
                    &format!("token{slot}"),
                    slot,
                    &format!("name{slot}"),
                )
                .await,
            );
        }
        let (a, b) = tokio::join!(
            bind(&store, "a", "token10", 10, "name10"),
            bind(&store, "a", "token11", 11, "name11")
        );
        assert!(matches!(
            (&a, &b),
            (GoogleBinding::Saved(_), GoogleBinding::LimitReached)
                | (GoogleBinding::LimitReached, GoogleBinding::Saved(_))
        ));
        saved(bind(&store, "a", "token1", 1, "name1").await);
        assert_eq!(
            store
                .google_subscriptions_for("a".into(), 100)
                .await
                .unwrap()
                .iter()
                .filter(|p| p.active)
                .count(),
            10
        );
    }

    #[tokio::test]
    async fn google_pending_never_grants_and_refund_shortens_access() {
        let store = Store::open(":memory:").unwrap();
        let mut pending = purchase(1, 500);
        pending.active = false;
        pending.state = "SUBSCRIPTION_STATE_PENDING".into();
        assert!(matches!(
            store
                .record_google_purchase("a".into(), "one".into(), pending, Some("alex".into()), 100)
                .await
                .unwrap(),
            GoogleBinding::Inactive
        ));
        assert!(store
            .google_subscriptions_for("a".into(), 100)
            .await
            .unwrap()
            .is_empty());
        saved(bind(&store, "a", "one", 1, "alex").await);
        let mut revoked = purchase(1, 500);
        revoked.active = false;
        revoked.state = "SUBSCRIPTION_STATE_EXPIRED".into();
        let row = saved(
            store
                .record_google_purchase("a".into(), "one".into(), revoked, None, 120)
                .await
                .unwrap(),
        );
        assert!(!row.active);
        assert_eq!(row.expires_at, 120);
        let expiry: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT expires_at FROM namespaces WHERE namespace='alex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(expiry, 120);
    }

    #[tokio::test]
    async fn google_linked_replacement_keeps_name_and_old_token_cannot_revoke_it() {
        let store = Store::open(":memory:").unwrap();
        saved(bind(&store, "a", "old", 1, "alex").await);
        let mut renewed = purchase(1, 900);
        renewed.linked_token = Some("old".into());
        let row = saved(
            store
                .record_google_purchase("a".into(), "new".into(), renewed, None, 200)
                .await
                .unwrap(),
        );
        assert_eq!(row.namespace.as_deref(), Some("alex"));
        let mut revoked = purchase(1, 250);
        revoked.active = false;
        revoked.state = "SUBSCRIPTION_STATE_EXPIRED".into();
        assert!(matches!(
            store
                .record_google_purchase("a".into(), "old".into(), revoked, None, 250)
                .await
                .unwrap(),
            GoogleBinding::Replaced
        ));
        let held = store
            .google_subscriptions_for("a".into(), 250)
            .await
            .unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].expires_at, 900);
    }

    #[tokio::test]
    async fn google_never_overwrites_an_existing_complimentary_or_paid_name() {
        let store = Store::open(":memory:").unwrap();
        store
            .set_namespace_owner("free-name".into(), "a".into(), "test", 100, None)
            .await
            .unwrap();
        assert!(saved(bind(&store, "a", "one", 1, "free-name").await)
            .namespace
            .is_none());
        saved(bind(&store, "a", "one", 1, "paid-name").await);
        assert!(saved(bind(&store, "a", "two", 2, "paid-name").await)
            .namespace
            .is_none());
        let source: String = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT source FROM namespaces WHERE namespace='free-name'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "test");
    }

    #[tokio::test]
    async fn lifecycle_google_terminal_checks_do_not_restart_recovery_or_revoke_a_new_owner() {
        let store = Store::open(":memory:").unwrap();
        let end = 1000;
        saved(
            store
                .record_google_purchase(
                    "old".into(),
                    "old-token".into(),
                    purchase(1, end),
                    Some("alex".into()),
                    end - 1,
                )
                .await
                .unwrap(),
        );
        let mut expired = purchase(1, end);
        expired.active = false;
        expired.state = "SUBSCRIPTION_STATE_EXPIRED".into();
        expired.auto_renewing = false;
        saved(
            store
                .record_google_purchase(
                    "old".into(),
                    "old-token".into(),
                    expired.clone(),
                    None,
                    end + 1,
                )
                .await
                .unwrap(),
        );
        let resale = end + super::super::lifecycle::RECOVERY_SECONDS as u64;
        saved(
            store
                .record_google_purchase(
                    "old".into(),
                    "old-token".into(),
                    expired.clone(),
                    None,
                    resale,
                )
                .await
                .unwrap(),
        );
        assert!(
            store
                .namespace_available("alex".into(), resale)
                .await
                .unwrap(),
            "polling must not move the recovery deadline"
        );
        let current = saved(
            store
                .record_google_purchase(
                    "new".into(),
                    "new-token".into(),
                    purchase(1, resale + 1000),
                    Some("alex".into()),
                    resale,
                )
                .await
                .unwrap(),
        );
        assert_eq!(current.namespace.as_deref(), Some("alex"));
        saved(
            store
                .record_google_purchase("old".into(), "old-token".into(), expired, None, resale + 1)
                .await
                .unwrap(),
        );
        assert_eq!(
            store
                .namespace_owner("alex".into(), resale + 1)
                .await
                .unwrap()
                .as_deref(),
            Some("new")
        );
        let renewed = saved(
            store
                .record_google_purchase(
                    "old".into(),
                    "old-token".into(),
                    purchase(1, resale + 1000),
                    None,
                    resale + 2,
                )
                .await
                .unwrap(),
        );
        assert!(
            renewed.active && renewed.namespace.is_none(),
            "a late renewal keeps paid credit without taking the new owner's name"
        );
    }
}
