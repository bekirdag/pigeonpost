//! One Apple subscription covers a fixed number of retained names. Legacy per-name receipts
//! stay in apple_subscriptions. Slot order makes downgrade behavior deterministic.
use super::{
    lifecycle, params, AppleClaim, AppleSubscription, Connection, OptionalExtension, Store,
    StoreError,
};
use crate::appstore::{plan_capacity, SubscriptionStatus};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS apple_handle_plans (
    account_id TEXT PRIMARY KEY,
    original_transaction_id TEXT NOT NULL UNIQUE,
    product_id TEXT NOT NULL,
    environment TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    active INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS apple_plan_names (
    account_id TEXT NOT NULL,
    slot INTEGER NOT NULL CHECK(slot BETWEEN 1 AND 10),
    namespace TEXT NOT NULL,
    PRIMARY KEY(account_id,slot),
    UNIQUE(account_id,namespace)
);
";

#[derive(Clone, Debug, serde::Serialize)]
pub struct ApplePlan {
    pub original_transaction_id: String,
    pub product_id: String,
    pub environment: String,
    pub expires_at: i64,
    pub capacity: usize,
    pub active: bool,
}

fn read_plan(c: &Connection, account: &str, now: u64) -> Result<Option<ApplePlan>, StoreError> {
    Ok(c.query_row(
        "SELECT original_transaction_id,product_id,environment,expires_at,active
        FROM apple_handle_plans WHERE account_id=?1",
        [account],
        |r| {
            let product: String = r.get(1)?;
            let expires_at: i64 = r.get(3)?;
            Ok(ApplePlan {
                original_transaction_id: r.get(0)?,
                capacity: plan_capacity(&product).unwrap_or(0),
                product_id: product,
                environment: r.get(2)?,
                expires_at,
                active: r.get::<_, bool>(4)? && expires_at > now as i64,
            })
        },
    )
    .optional()?)
}

/// Apply only a fresh provider snapshot, under the shared Apple verification lock.
fn install(
    c: &Connection,
    account: &str,
    status: &SubscriptionStatus,
    now: u64,
) -> Result<AppleClaim, StoreError> {
    let e = &status.entitlement;
    let capacity = plan_capacity(&e.product_id).ok_or(StoreError::Corrupt("unknown Apple plan"))?;
    if e.app_account_token
        .as_deref()
        .is_none_or(|token| !token.eq_ignore_ascii_case(&crate::appstore::account_token(account)))
    {
        return Ok(AppleClaim::SubscriptionBoundElsewhere);
    }
    let owner: Option<String> = c
        .query_row(
            "SELECT account_id FROM apple_handle_plans
        WHERE original_transaction_id=?1",
            [&e.original_transaction_id],
            |r| r.get(0),
        )
        .optional()?;
    if owner.as_deref().is_some_and(|owner| owner != account) {
        return Ok(AppleClaim::SubscriptionBoundElsewhere);
    }
    if let Some(current) = read_plan(c, account, now)? {
        if current.original_transaction_id != e.original_transaction_id && current.active {
            return Ok(AppleClaim::NamespaceAlreadySubscribed);
        }
    }
    c.execute("INSERT INTO apple_handle_plans
        (account_id,original_transaction_id,product_id,environment,expires_at,active,updated_at)
        VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id) DO UPDATE SET
        original_transaction_id=?2,product_id=?3,environment=?4,expires_at=?5,active=?6,updated_at=?7",
        params![account, e.original_transaction_id, e.product_id, e.environment, e.expires_at, status.active, now as i64])?;
    let provider = format!("plan:{account}");
    // A downgrade keeps the first N slots. Other names stop working and enter the existing
    // recovery period; their mailbox contents and ownership are never transferred by this update.
    c.execute("UPDATE namespaces SET
        expires_at=CASE WHEN ?1 AND (SELECT slot FROM apple_plan_names s WHERE s.account_id=?2 AND s.namespace=namespaces.namespace)<=?3
            THEN ?4 ELSE MIN(COALESCE(expires_at,?5),?5) END,
        release_at=CASE WHEN ?1 AND (SELECT slot FROM apple_plan_names s WHERE s.account_id=?2 AND s.namespace=namespaces.namespace)<=?3 THEN NULL
            WHEN ?6 OR (SELECT slot FROM apple_plan_names s WHERE s.account_id=?2 AND s.namespace=namespaces.namespace)>?3
            THEN COALESCE(release_at,?5+?7) ELSE NULL END,
        verified_at=?5 WHERE account_id=?2 AND source='apple' AND provider_ref=?8
        AND EXISTS(SELECT 1 FROM apple_plan_names s WHERE s.account_id=?2 AND s.namespace=namespaces.namespace)",
        params![status.active,account,capacity as i64,e.expires_at,now as i64,status.terminal,lifecycle::RECOVERY_SECONDS,provider])?;
    Ok(AppleClaim::Renewed)
}

impl Store {
    pub async fn apple_plan(
        &self,
        account: String,
        now: u64,
    ) -> Result<Option<ApplePlan>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            read_plan(&conn.lock().expect("store lock"), &account, now)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn apply_apple_plan(
        &self,
        account: String,
        status: SubscriptionStatus,
        namespace: Option<String>,
        now: u64,
    ) -> Result<AppleClaim, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let result = install(&tx, &account, &status, now)?;
            if result != AppleClaim::Renewed { return Ok(result); }
            let Some(name) = namespace else { tx.commit()?; return Ok(result); };
            let capacity = plan_capacity(&status.entitlement.product_id).unwrap_or(0);
            if !status.active || status.entitlement.expires_at <= now as i64 { tx.commit()?; return Ok(AppleClaim::LimitReached); }
            let provider = format!("plan:{account}");
            let existing: Option<i64> = tx.query_row("SELECT s.slot FROM apple_plan_names s JOIN namespaces n
                ON n.namespace=s.namespace AND n.account_id=s.account_id AND n.source='apple' AND n.provider_ref=?3
                WHERE s.account_id=?1 AND s.namespace=?2",params![account,name,provider],|r| r.get(0)).optional()?;
            if let Some(slot) = existing {
                tx.commit()?;
                return Ok(if slot <= capacity as i64 { AppleClaim::Renewed } else { AppleClaim::LimitReached });
            }
            let other: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM namespaces WHERE namespace=?1
                AND account_id=?2 AND (expires_at IS NULL OR expires_at>?3))",params![name,account,now as i64],|r|r.get(0))?;
            if other { tx.commit()?; return Ok(AppleClaim::NamespaceAlreadySubscribed); }
            if !lifecycle::available_to(&tx,&name,Some(&account),now)? {
                tx.commit()?; return Ok(AppleClaim::NamespaceTaken);
            }
            let mut slot = None;
            for candidate in 1..=capacity {
                let occupied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM apple_plan_names s JOIN namespaces n
                    ON n.namespace=s.namespace AND n.account_id=s.account_id AND n.source='apple' AND n.provider_ref=?3
                    WHERE s.account_id=?1 AND s.slot=?2)",params![account,candidate as i64,provider],|r|r.get(0))?;
                if !occupied { slot=Some(candidate); break; }
            }
            let Some(slot) = slot else { tx.commit()?; return Ok(AppleClaim::LimitReached); };
            lifecycle::detach_previous_aliases(&tx,&name,&account)?;
            // Discard only stale slot records whose namespace has already changed ownership.
            tx.execute("DELETE FROM apple_plan_names WHERE account_id=?1 AND (slot=?2 OR namespace=?3)",params![account,slot as i64,name])?;
            tx.execute("INSERT INTO apple_plan_names(account_id,slot,namespace) VALUES(?1,?2,?3)",params![account,slot as i64,name])?;
            tx.execute("INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at,provider_ref,release_at)
                VALUES(?1,?2,'apple',?3,?4,?5,NULL) ON CONFLICT(namespace) DO UPDATE SET
                account_id=?2,source='apple',verified_at=?3,expires_at=?4,provider_ref=?5,release_at=NULL",
                params![name,account,now as i64,status.entitlement.expires_at,provider])?;
            tx.commit()?;
            Ok(AppleClaim::Granted)
        }).await.map_err(|_| StoreError::Join)?
    }

    pub async fn apple_plan_handles(
        &self,
        account: String,
        now: u64,
    ) -> Result<Vec<AppleSubscription>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().expect("store lock");
            let mut stmt=c.prepare("SELECT p.original_transaction_id,s.namespace,p.product_id,p.environment,n.expires_at,
                p.active AND p.expires_at>?2 AND n.expires_at>?2 FROM apple_plan_names s
                JOIN apple_handle_plans p ON p.account_id=s.account_id
                JOIN namespaces n ON n.namespace=s.namespace AND n.account_id=s.account_id AND n.source='apple' AND n.provider_ref=?3
                WHERE s.account_id=?1 ORDER BY s.slot")?;
            let values=stmt.query_map(params![account,now as i64,format!("plan:{account}")],|r|Ok(AppleSubscription {
                original_transaction_id:r.get(0)?,namespace:r.get(1)?,product_id:r.get(2)?,environment:r.get(3)?,expires_at:r.get(4)?,active:r.get(5)?
            }))?.collect::<Result<Vec<_>,_>>()?;
            Ok(values)
        }).await.map_err(|_|StoreError::Join)?
    }

    pub async fn apple_plans_due(&self, now: u64) -> Result<Vec<(String, ApplePlan)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c=conn.lock().expect("store lock");
            let mut stmt=c.prepare("SELECT account_id FROM apple_handle_plans WHERE updated_at<?1 ORDER BY updated_at LIMIT 100")?;
            let accounts=stmt.query_map([now.saturating_sub(300) as i64],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let mut result=Vec::new();
            for account in accounts { if let Some(plan)=read_plan(&c,&account,now)? {result.push((account,plan));} }
            Ok(result)
        }).await.map_err(|_|StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstore::{account_token, Entitlement};

    fn status(capacity: usize, expiry: i64, active: bool, account: &str) -> SubscriptionStatus {
        SubscriptionStatus {
            entitlement: Entitlement {
                original_transaction_id: "plan-origin".into(),
                product_id: format!("dev.pigeonpost.inbox.handles.{capacity}.yearly"),
                app_account_token: Some(account_token(account)),
                expires_at: expiry,
                environment: "Sandbox".into(),
            },
            active,
            terminal: !active,
            revoked: false,
        }
    }
    async fn apply(s: &Store, capacity: usize, name: Option<&str>, now: u64) -> AppleClaim {
        s.apply_apple_plan(
            "acct-a".into(),
            status(capacity, 10_000_000, true, "acct-a"),
            name.map(str::to_owned),
            now,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn one_plan_registers_capacity_then_upgrade_retains_all_names() {
        let s = Store::open(":memory:").unwrap();
        assert_eq!(apply(&s, 1, None, 100).await, AppleClaim::Renewed);
        assert_eq!(apply(&s, 1, Some("first"), 100).await, AppleClaim::Granted);
        assert_eq!(
            apply(&s, 1, Some("second"), 100).await,
            AppleClaim::LimitReached
        );
        assert_eq!(apply(&s, 2, Some("second"), 101).await, AppleClaim::Granted);
        let names = s.apple_plan_handles("acct-a".into(), 101).await.unwrap();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| n.active));
        assert!(names
            .iter()
            .all(|n| n.original_transaction_id == "plan-origin"));
        assert_eq!(
            s.apple_plan("acct-a".into(), 101)
                .await
                .unwrap()
                .unwrap()
                .capacity,
            2
        );
        assert!(s
            .apple_subscriptions_for("acct-a".into(), 101)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn ten_slots_have_one_payment_identity_and_concurrent_claims_cannot_overfill() {
        let s = Store::open(":memory:").unwrap();
        for n in 1..=9 {
            assert_eq!(
                apply(&s, 10, Some(&format!("name{n}")), 100).await,
                AppleClaim::Granted
            );
        }
        let (a, b) = tokio::join!(
            apply(&s, 10, Some("tenth"), 100),
            apply(&s, 10, Some("eleventh"), 100)
        );
        assert!(matches!(
            (a, b),
            (AppleClaim::Granted, AppleClaim::LimitReached)
                | (AppleClaim::LimitReached, AppleClaim::Granted)
        ));
        assert_eq!(
            s.apple_plan_handles("acct-a".into(), 100)
                .await
                .unwrap()
                .len(),
            10
        );
    }

    #[tokio::test]
    async fn downgrade_and_expiry_retain_history_and_restore_reactivates_only_covered_names() {
        let s = Store::open(":memory:").unwrap();
        apply(&s, 2, Some("first"), 100).await;
        apply(&s, 2, Some("second"), 100).await;
        apply(&s, 1, None, 101).await;
        let names = s.apple_plan_handles("acct-a".into(), 101).await.unwrap();
        assert!(names[0].active);
        assert!(!names[1].active);
        assert!(!s.namespace_available("second".into(), 101).await.unwrap());
        assert_eq!(
            apply(&s, 1, Some("second"), 101).await,
            AppleClaim::LimitReached
        );
        apply(&s, 2, None, 102).await;
        assert!(s
            .apple_plan_handles("acct-a".into(), 102)
            .await
            .unwrap()
            .iter()
            .all(|n| n.active));
        s.apply_apple_plan("acct-a".into(), status(2, 103, false, "acct-a"), None, 103)
            .await
            .unwrap();
        assert!(
            !s.apple_plan("acct-a".into(), 103)
                .await
                .unwrap()
                .unwrap()
                .active
        );
        assert!(s
            .apple_plan_handles("acct-a".into(), 103)
            .await
            .unwrap()
            .iter()
            .all(|n| !n.active));
        apply(&s, 2, None, 104).await;
        assert!(s
            .apple_plan_handles("acct-a".into(), 104)
            .await
            .unwrap()
            .iter()
            .all(|n| n.active));
    }

    #[tokio::test]
    async fn snapshots_and_receipts_cannot_cross_accounts_or_stack_plans() {
        let s = Store::open(":memory:").unwrap();
        apply(&s, 1, Some("first"), 100).await;
        assert_eq!(
            s.apply_apple_plan(
                "acct-b".into(),
                status(10, 10_000_000, true, "acct-a"),
                Some("stolen".into()),
                101
            )
            .await
            .unwrap(),
            AppleClaim::SubscriptionBoundElsewhere
        );
        let mut other = status(10, 10_000_000, true, "acct-a");
        other.entitlement.original_transaction_id = "another-apple-account".into();
        assert_eq!(
            s.apply_apple_plan("acct-a".into(), other, None, 101)
                .await
                .unwrap(),
            AppleClaim::NamespaceAlreadySubscribed
        );
        assert_eq!(
            s.apple_plan("acct-a".into(), 101)
                .await
                .unwrap()
                .unwrap()
                .capacity,
            1
        );
    }

    #[tokio::test]
    async fn recovery_cannot_take_a_resold_name_back_and_can_fill_the_vacancy() {
        let s = Store::open(":memory:").unwrap();
        apply(&s, 1, Some("first"), 100).await;
        s.apply_apple_plan("acct-a".into(), status(1, 101, false, "acct-a"), None, 101)
            .await
            .unwrap();
        let later = 102 + lifecycle::RECOVERY_SECONDS as u64;
        s.apply_apple_plan(
            "acct-a".into(),
            status(1, 101, false, "acct-a"),
            None,
            later,
        )
        .await
        .unwrap();
        assert!(s
            .set_namespace_owner("first".into(), "new-owner".into(), "grant", later, None)
            .await
            .unwrap());
        apply(&s, 1, None, later + 1).await;
        assert!(s
            .apple_plan_handles("acct-a".into(), later + 1)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            apply(&s, 1, Some("first"), later + 1).await,
            AppleClaim::NamespaceTaken
        );
        assert_eq!(
            apply(&s, 1, Some("replacement"), later + 1).await,
            AppleClaim::Granted
        );
        assert_eq!(
            s.apple_plan_handles("acct-a".into(), later + 1)
                .await
                .unwrap()[0]
                .namespace,
            "replacement"
        );
    }

    #[test]
    fn plan_identifiers_are_exact_and_legacy_receipts_are_not_plans() {
        for n in 1..=10 {
            assert_eq!(
                plan_capacity(&format!("dev.pigeonpost.inbox.handles.{n}.yearly")),
                Some(n)
            );
        }
        for id in [
            "dev.pigeonpost.inbox.handle.yearly",
            "dev.pigeonpost.inbox.handles.0.yearly",
            "dev.pigeonpost.inbox.handles.11.yearly",
            "dev.pigeonpost.inbox.handles.01.yearly",
        ] {
            assert_eq!(plan_capacity(id), None);
        }
    }
}
