//! Minimal, persistence-ready organisations (SDS §6.1) for the billing bridge: a draft created
//! before checkout, activated after verified payment, then kept in step with its entitlement.
//! Phase 4 adds members, roles, agents and delegations to this table.
use super::entitlements::{self, Entitlement, EntitlementInput, UpsertOutcome};
use super::{lifecycle, params, Connection, OptionalExtension, Store, StoreError};
use serde::Serialize;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS orgs (
    id TEXT PRIMARY KEY,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    logo_avatar_id TEXT,
    owner_account TEXT NOT NULL,
    internal_account_id TEXT UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('pending','active','past_due','read_only','suspended','deleted','paid_unprovisioned')),
    seats INTEGER NOT NULL CHECK (seats > 0),
    agent_limit INTEGER NOT NULL CHECK (agent_limit = seats * 10),
    storage_bytes INTEGER NOT NULL,
    plan_code TEXT,
    billing_ref TEXT,
    period_end INTEGER,
    settings TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    deleted_at INTEGER,
    create_operation_id TEXT NOT NULL UNIQUE,
    create_hash TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS orgs_live_namespace ON orgs(namespace)
    WHERE status IN ('active','past_due','read_only','suspended');
CREATE INDEX IF NOT EXISTS orgs_by_owner ON orgs(owner_account);
";

/// Pooled storage per seat (SDS §8.3 proposal): 5 GB.
pub const STORAGE_PER_SEAT: i64 = 5 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Org {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub owner_account: String,
    pub status: String,
    pub seats: i64,
    pub agent_limit: i64,
    pub storage_bytes: i64,
    pub plan_code: Option<String>,
    pub billing_ref: Option<String>,
    pub period_end: Option<i64>,
    pub created_at: i64,
}

const COLUMNS: &str =
    "id,namespace,name,owner_account,status,seats,agent_limit,storage_bytes,plan_code,billing_ref,period_end,created_at";

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Org> {
    Ok(Org {
        id: r.get(0)?,
        namespace: r.get(1)?,
        name: r.get(2)?,
        owner_account: r.get(3)?,
        status: r.get(4)?,
        seats: r.get(5)?,
        agent_limit: r.get(6)?,
        storage_bytes: r.get(7)?,
        plan_code: r.get(8)?,
        billing_ref: r.get(9)?,
        period_end: r.get(10)?,
        created_at: r.get(11)?,
    })
}

fn get(c: &Connection, id: &str) -> Result<Option<Org>, StoreError> {
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM orgs WHERE id=?1"),
        [id],
        read,
    )
    .optional()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgDraft {
    pub operation_id: String,
    pub owner_account: String,
    pub name: String,
    pub namespace: String,
    pub plan_code: String,
    pub seats: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftOutcome {
    Created(Org),
    Replayed(Org),
    OperationConflict,
    OwnerUnknown,
    NamespaceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum ActivateOutcome {
    Done(Org, Entitlement),
    OrgUnknown,
    EntitlementInactive,
    Entitlement(UpsertOutcome),
}

/// Keep an activated org's status and limits in step with its best entitlement. Drafts and
/// paid-but-unprovisioned orgs keep their status: only activation provisions a namespace. A
/// provisioned org follows the D-14 lapse timeline (`crate::org::lifecycle_status`).
pub(super) fn follow_entitlement(c: &Connection, org: &str, now: u64) -> Result<(), StoreError> {
    let Some(current) = get(c, org)? else {
        return Ok(());
    };
    if current.status == "deleted" {
        return Ok(());
    }
    let rows = entitlements::for_subject(c, "org", org, now)?;
    let best = rows
        .iter()
        .filter(|e| e.active)
        .max_by_key(|e| (e.seats.unwrap_or(0), e.paid_through.unwrap_or(i64::MAX)))
        .or_else(|| rows.iter().max_by_key(|e| e.paid_through.unwrap_or(0)));
    let Some(best) = best else {
        return Ok(());
    };
    let provisioned = matches!(
        current.status.as_str(),
        "active" | "past_due" | "read_only" | "suspended"
    );
    let status = crate::org::lifecycle_status(Some(best), &current.status, now);
    let seats = best.seats.unwrap_or(current.seats).max(1);
    c.execute(
        "UPDATE orgs SET status=CASE WHEN ?2 THEN ?3 ELSE status END,
            status_since=CASE WHEN ?2 AND status<>?3 THEN ?9 ELSE status_since END,
            seats=?4, agent_limit=?4*10,
            storage_bytes=?5, plan_code=?6, billing_ref=?7, period_end=?8 WHERE id=?1",
        params![
            org,
            provisioned,
            status,
            seats,
            best.storage_bytes.unwrap_or(seats * STORAGE_PER_SEAT),
            best.plan_code,
            best.provider_ref,
            best.paid_through,
            now as i64
        ],
    )?;
    Ok(())
}

fn draft_hash(d: &OrgDraft) -> String {
    use sha2::{Digest, Sha256};
    let text = serde_json::json!([d.owner_account, d.name, d.namespace, d.plan_code, d.seats]);
    Sha256::digest(text.to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Store {
    pub async fn create_org_draft(
        &self,
        draft: OrgDraft,
        now: u64,
    ) -> Result<DraftOutcome, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let hash = draft_hash(&draft);
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT id,create_hash FROM orgs WHERE create_operation_id=?1",
                    [&draft.operation_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, prior_hash)) = prior {
                if prior_hash != hash {
                    return Ok(DraftOutcome::OperationConflict);
                }
                let org = get(&tx, &id)?.ok_or(StoreError::Corrupt("org disappeared"))?;
                return Ok(DraftOutcome::Replayed(org));
            }
            let owner: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
                [&draft.owner_account],
                |r| r.get(0),
            )?;
            if !owner {
                return Ok(DraftOutcome::OwnerUnknown);
            }
            let live_org: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM orgs WHERE namespace=?1 AND status IN ('active','past_due','read_only','suspended'))",
                [&draft.namespace],
                |r| r.get(0),
            )?;
            if live_org || !lifecycle::available_to(&tx, &draft.namespace, None, now)? {
                return Ok(DraftOutcome::NamespaceUnavailable);
            }
            let id: String = tx.query_row("SELECT 'org_'||lower(hex(randomblob(13)))", [], |r| r.get(0))?;
            tx.execute(
                "INSERT INTO orgs (id,namespace,name,owner_account,status,seats,agent_limit,storage_bytes,plan_code,
                    created_at,create_operation_id,create_hash)
                 VALUES (?1,?2,?3,?4,'pending',?5,?5*10,?6,?7,?8,?9,?10)",
                params![
                    id,
                    draft.namespace,
                    draft.name,
                    draft.owner_account,
                    draft.seats,
                    draft.seats * STORAGE_PER_SEAT,
                    draft.plan_code,
                    now as i64,
                    draft.operation_id,
                    hash
                ],
            )?;
            let org = get(&tx, &id)?.ok_or(StoreError::Corrupt("org disappeared"))?;
            tx.commit()?;
            Ok(DraftOutcome::Created(org))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Activate a paid draft: create the org's non-login custodian account and give it the org
    /// namespace — unless somebody else claimed the name meanwhile, in which case the org is
    /// `paid_unprovisioned` and the payment is kept, never the name taken.
    pub async fn activate_org(
        &self,
        org_id: String,
        entitlement: EntitlementInput,
        now: u64,
    ) -> Result<ActivateOutcome, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let Some(org) = get(&tx, &org_id)? else {
                return Ok(ActivateOutcome::OrgUnknown);
            };
            let live = matches!(entitlement.status.as_str(), "active" | "past_due" | "grace" | "canceled")
                && entitlement.paid_through.is_none_or(|p| p > now as i64);
            if org.status == "pending" && !live {
                return Ok(ActivateOutcome::EntitlementInactive);
            }
            if org.status == "pending" {
                let internal: String = match tx
                    .query_row("SELECT internal_account_id FROM orgs WHERE id=?1", [&org_id], |r| {
                        r.get::<_, Option<String>>(0)
                    })? {
                    Some(id) => id,
                    None => {
                        let id: String = tx.query_row(
                            "SELECT 'acct_org_'||lower(hex(randomblob(12)))",
                            [],
                            |r| r.get(0),
                        )?;
                        tx.execute(
                            "INSERT INTO accounts(id,created_at,oidc_sub) VALUES(?1,?2,NULL)",
                            params![id, now as i64],
                        )?;
                        tx.execute(
                            "UPDATE orgs SET internal_account_id=?2 WHERE id=?1",
                            params![org_id, id],
                        )?;
                        id
                    }
                };
                let live_org: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM orgs WHERE namespace=?1 AND id!=?2 AND status IN ('active','past_due','read_only','suspended'))",
                    params![org.namespace, org_id],
                    |r| r.get(0),
                )?;
                let status = if !live_org
                    && lifecycle::available_to(&tx, &org.namespace, Some(&internal), now)?
                {
                    lifecycle::detach_previous_aliases(&tx, &org.namespace, &internal)?;
                    tx.execute(
                        "INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at,provider_ref,release_at)
                         VALUES(?1,?2,'org',?3,NULL,?4,NULL) ON CONFLICT(namespace) DO UPDATE SET
                         account_id=?2,source='org',verified_at=?3,expires_at=NULL,provider_ref=?4,release_at=NULL",
                        params![org.namespace, internal, now as i64, format!("org:{org_id}")],
                    )?;
                    "active"
                } else {
                    "paid_unprovisioned"
                };
                tx.execute(
                    "UPDATE orgs SET status=?2, status_since=?3 WHERE id=?1",
                    params![org_id, status, now as i64],
                )?;
                if status == "active" {
                    crate::org::provision_in(&tx, &org_id, now)?;
                }
            }
            let mut input = entitlement;
            input.subject_kind = "org".into();
            input.subject_id = org_id.clone();
            let saved = match entitlements::upsert(&tx, &input, now)? {
                UpsertOutcome::Applied(e) | UpsertOutcome::Duplicate(e) | UpsertOutcome::Stale(e) => {
                    follow_entitlement(&tx, &org_id, now)?;
                    e
                }
                other => return Ok(ActivateOutcome::Entitlement(other)),
            };
            let org = get(&tx, &org_id)?.ok_or(StoreError::Corrupt("org disappeared"))?;
            tx.commit()?;
            Ok(ActivateOutcome::Done(org, saved))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Re-derive every provisioned org's lifecycle status from its entitlements (the worker runs
    /// this periodically so lapses advance without a billing event). Returns the orgs whose status
    /// changed, with their old and new status.
    pub async fn org_lifecycle_sweep(
        &self,
        now: u64,
    ) -> Result<Vec<(String, String, String)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let ids: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, status FROM orgs WHERE status IN ('active','past_due','read_only','suspended')",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            let mut changed = Vec::new();
            for (id, before) in ids {
                follow_entitlement(&tx, &id, now)?;
                let after: String =
                    tx.query_row("SELECT status FROM orgs WHERE id=?1", [&id], |r| r.get(0))?;
                if after != before {
                    changed.push((id, before, after));
                }
            }
            tx.commit()?;
            Ok(changed)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn org(&self, id: String) -> Result<Option<Org>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || get(&conn.lock().expect("store lock"), &id))
            .await
            .map_err(|_| StoreError::Join)?
    }
}
