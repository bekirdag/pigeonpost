//! Paid names are renewable aliases. Mailbox keys and history never change owners with a name.
use super::{params, Connection, OptionalExtension, Store, StoreError, StoredIdentity};

pub(super) const RECOVERY_SECONDS: i64 = 30 * 86400;
const VERIFICATION_FRESHNESS: i64 = 600;

fn paid_namespace(namespace: &str) -> bool {
    !crate::PROVIDER_NAMESPACES.contains(&namespace)
        && !crate::OPEN_NAMESPACES.contains(&namespace)
        && !namespace.contains('@')
}

pub(super) fn may_mint(
    c: &Connection,
    namespace: &str,
    account: Option<&str>,
    now: u64,
) -> Result<bool, StoreError> {
    let namespace = namespace.trim_start_matches('/');
    if !paid_namespace(namespace) {
        return Ok(true);
    }
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM namespaces WHERE namespace=?1
        AND account_id=?2 AND (expires_at IS NULL OR expires_at>?3))",
        params![namespace, account, now as i64],
        |r| r.get(0),
    )?)
}

pub(super) fn visible_handle(
    c: &Connection,
    handle: Option<String>,
    account: Option<&str>,
    now: u64,
) -> Result<Option<String>, StoreError> {
    let Some(handle) = handle else {
        return Ok(None);
    };
    let namespace = handle
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    Ok(if may_mint(c, namespace, account, now)? {
        Some(handle)
    } else {
        None
    })
}

pub(super) fn visible_identity(
    c: &Connection,
    mut id: StoredIdentity,
) -> Result<StoredIdentity, StoreError> {
    id.handle = visible_handle(c, id.handle, id.account_id.as_deref(), crate::now_unix())?;
    Ok(id)
}

/// Unknown provider state keeps a name reserved. Local expiry alone is never proof of cancellation.
pub(super) fn available_to(
    c: &Connection,
    namespace: &str,
    account: Option<&str>,
    now: u64,
) -> Result<bool, StoreError> {
    let row: Option<(String, Option<i64>, Option<i64>, i64)> = c.query_row(
        "SELECT account_id,expires_at,release_at,verified_at FROM namespaces WHERE namespace=?1",
        [namespace], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
    Ok(match row {
        None => true,
        Some((owner, expiry, release, verified)) => {
            account == Some(owner.as_str())
                || (expiry.is_some_and(|v| v <= now as i64)
                    && release.is_some_and(|v| v <= now as i64)
                    && verified >= (now as i64).saturating_sub(VERIFICATION_FRESHNESS))
        }
    })
}

/// Call only after the ownership check, inside the same write transaction as the new grant.
/// Clear aliases, never identities, capabilities, account ownership, contacts, or messages.
pub(super) fn detach_previous_aliases(
    c: &Connection,
    namespace: &str,
    account: &str,
) -> Result<(), StoreError> {
    let transferred: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM namespaces WHERE namespace=?1 AND account_id!=?2)",
        params![namespace, account],
        |r| r.get(0),
    )?;
    if transferred {
        // A buyer inherits the spelling, never the previous owner's permission to drive agents.
        // Keep contact labels/admission and explicit key-address grants, but require fresh grants
        // for this namespace. Empty verbs also defeat auto_accept_known's autonomy shortcut.
        c.execute(
            "UPDATE contacts SET autonomy='review',allowed_verbs='[]',updated_at=?1
            WHERE peer=?2 OR peer GLOB ?3",
            params![
                crate::now_unix() as i64,
                format!("/{namespace}"),
                format!("/{namespace}/*")
            ],
        )?;
    }
    c.execute(
        "UPDATE identities SET handle=NULL WHERE handle GLOB ?1
        AND (account_id IS NULL OR account_id!=?2)",
        params![format!("/{namespace}/*"), account],
    )?;
    Ok(())
}

impl Store {
    pub async fn namespace_available(
        &self,
        namespace: String,
        now: u64,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            available_to(&conn.lock().expect("store lock"), &namespace, None, now)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn namespace_recoverable_by(
        &self,
        namespace: String,
        account: String,
        now: u64,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE namespace=?1
                AND account_id=?2 AND expires_at<=?3)",
                params![namespace, account, now as i64],
                |r| r.get(0),
            )?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn apple_due(&self, now: u64) -> Result<Vec<AppleRefresh>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().expect("store lock");
            let mut stmt = c.prepare("SELECT s.original_transaction_id,s.account_id,s.namespace,s.environment
                FROM apple_subscriptions s JOIN namespaces n ON n.namespace=s.namespace
                AND n.account_id=s.account_id AND n.source='apple' AND n.provider_ref=s.original_transaction_id
                WHERE s.updated_at<?1 ORDER BY s.updated_at LIMIT 100")?;
            let rows = stmt.query_map([now.saturating_sub(300) as i64], |r| Ok(AppleRefresh {
                original: r.get(0)?, account: r.get(1)?, namespace: r.get(2)?, environment: r.get(3)?,
            }))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await.map_err(|_| StoreError::Join)?
    }

    /// A provider snapshot can shorten entitlement after revocation, but only its current binding.
    pub async fn refresh_apple(
        &self,
        row: AppleRefresh,
        status: crate::appstore::SubscriptionStatus,
        now: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let expiry = status.entitlement.expires_at;
            if status.entitlement.original_transaction_id != row.original
                || status.entitlement.environment != row.environment
            {
                return Err(StoreError::Corrupt("Apple refresh binding mismatch"));
            }
            tx.execute(
                "UPDATE apple_subscriptions SET expires_at=?1,updated_at=?2,product_id=?3
                WHERE original_transaction_id=?4 AND account_id=?5",
                params![
                    expiry,
                    now as i64,
                    status.entitlement.product_id,
                    row.original,
                    row.account
                ],
            )?;
            tx.execute(
                "UPDATE namespaces SET expires_at=?1,verified_at=?2,release_at=?3
                WHERE namespace=?4 AND account_id=?5 AND source='apple' AND provider_ref=?6",
                params![
                    expiry,
                    now as i64,
                    status.terminal.then_some(expiry + RECOVERY_SECONDS),
                    row.namespace,
                    row.account,
                    row.original
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

pub struct AppleRefresh {
    pub original: String,
    pub account: String,
    pub namespace: String,
    pub environment: String,
}
