//! Unified entitlements (SDS §8.8): one row per provider-bound grant of capacity, whatever paid
//! for it. The provider tables (`apple_handle_plans`, `apple_subscriptions`, `google_subscriptions`)
//! stay the record of provider state; their write paths mirror into this table inside the same
//! transaction, and `backfill` re-derives every mirrored row at startup. Web (iyzico), grant and
//! test rows arrive only through the internal API, with a caller-supplied reconciliation version.
//!
//! Web plan names: an account's iyzico/grant/test entitlements give it `capacity = MAX(names)`
//! (overlapping subscriptions never add up). Names live in `plan_names` slots, exactly like
//! `apple_plan_names`, and in `namespaces` as `source='web'`, `provider_ref='web:<account>'`.
use super::{lifecycle, params, Connection, OptionalExtension, Store, StoreError};
use serde::Serialize;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entitlements (
    id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('account','org')),
    subject_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('apple','google','iyzico','grant','test')),
    plan_code TEXT NOT NULL,
    plan_version TEXT NOT NULL,
    names INTEGER,
    seats INTEGER,
    agent_limit INTEGER,
    storage_bytes INTEGER,
    status TEXT NOT NULL CHECK (status IN ('active','past_due','grace','expired','canceled')),
    period_end INTEGER,
    paid_through INTEGER,
    provider_ref TEXT NOT NULL,
    provider_environment TEXT NOT NULL,
    cancel_requested_at INTEGER,
    pending_plan_code TEXT,
    pending_effective_at INTEGER,
    updated_at INTEGER NOT NULL,
    reconciliation_version INTEGER NOT NULL,
    UNIQUE (source, provider_environment, provider_ref)
);
CREATE INDEX IF NOT EXISTS entitlements_by_subject ON entitlements(subject_kind, subject_id);
CREATE TABLE IF NOT EXISTS plan_names (
    account_id TEXT NOT NULL,
    slot INTEGER NOT NULL CHECK(slot BETWEEN 1 AND 10),
    namespace TEXT NOT NULL,
    PRIMARY KEY(account_id,slot),
    UNIQUE(account_id,namespace)
);
";

/// Plan code of entitlements mirrored from legacy `PUT /v1/namespaces` grants. They already carry
/// their one name, so they never add web plan capacity.
pub const LEGACY_GRANT_PLAN: &str = "legacy-namespace-grant";
pub const MAX_WEB_NAMES: i64 = 10;
/// Statuses that still give access while `paid_through` has not passed.
const LIVE: &str = "status IN ('active','past_due','grace','canceled') AND (paid_through IS NULL OR paid_through > ?2)";
const COLUMNS: &str = "id,subject_kind,subject_id,source,plan_code,plan_version,names,seats,agent_limit,storage_bytes,status,period_end,paid_through,provider_ref,provider_environment,cancel_requested_at,pending_plan_code,pending_effective_at,updated_at,reconciliation_version";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Entitlement {
    pub id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub source: String,
    pub plan_code: String,
    pub plan_version: String,
    pub names: Option<i64>,
    pub seats: Option<i64>,
    pub agent_limit: Option<i64>,
    pub storage_bytes: Option<i64>,
    pub status: String,
    pub period_end: Option<i64>,
    pub paid_through: Option<i64>,
    pub provider_ref: String,
    pub provider_environment: String,
    pub cancel_requested_at: Option<i64>,
    pub pending_plan_code: Option<String>,
    pub pending_effective_at: Option<i64>,
    pub updated_at: i64,
    pub reconciliation_version: i64,
    pub active: bool,
}

fn read_row(r: &rusqlite::Row<'_>, now: u64) -> rusqlite::Result<Entitlement> {
    let status: String = r.get(10)?;
    let paid_through: Option<i64> = r.get(12)?;
    let active = matches!(
        status.as_str(),
        "active" | "past_due" | "grace" | "canceled"
    ) && paid_through.is_none_or(|p| p > now as i64);
    Ok(Entitlement {
        id: r.get(0)?,
        subject_kind: r.get(1)?,
        subject_id: r.get(2)?,
        source: r.get(3)?,
        plan_code: r.get(4)?,
        plan_version: r.get(5)?,
        names: r.get(6)?,
        seats: r.get(7)?,
        agent_limit: r.get(8)?,
        storage_bytes: r.get(9)?,
        status,
        period_end: r.get(11)?,
        paid_through,
        provider_ref: r.get(13)?,
        provider_environment: r.get(14)?,
        cancel_requested_at: r.get(15)?,
        pending_plan_code: r.get(16)?,
        pending_effective_at: r.get(17)?,
        updated_at: r.get(18)?,
        reconciliation_version: r.get(19)?,
        active,
    })
}

pub(super) fn lookup(
    c: &Connection,
    source: &str,
    env: &str,
    reference: &str,
    now: u64,
) -> Result<Option<Entitlement>, StoreError> {
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM entitlements WHERE source=?1 AND provider_environment=?2 AND provider_ref=?3"),
        params![source, env, reference],
        |r| read_row(r, now),
    )
    .optional()?)
}

pub(super) fn for_subject(
    c: &Connection,
    kind: &str,
    id: &str,
    now: u64,
) -> Result<Vec<Entitlement>, StoreError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {COLUMNS} FROM entitlements WHERE subject_kind=?1 AND subject_id=?2 ORDER BY updated_at DESC, id"
    ))?;
    let rows = stmt
        .query_map(params![kind, id], |r| read_row(r, now))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// What a caller (the billing bridge) asserts about one provider-bound entitlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementInput {
    pub source: String,
    pub environment: String,
    pub reference: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub plan_code: String,
    pub plan_version: String,
    pub names: Option<i64>,
    pub seats: Option<i64>,
    pub agent_limit: Option<i64>,
    pub storage_bytes: Option<i64>,
    pub status: String,
    pub period_end: Option<i64>,
    pub paid_through: Option<i64>,
    pub cancel_requested_at: Option<i64>,
    pub pending_plan_code: Option<String>,
    pub pending_effective_at: Option<i64>,
    pub reconciliation_version: i64,
    /// Only for account subjects: the account's OIDC subject must equal this.
    pub expected_oidc_sub: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    Applied(Entitlement),
    Stale(Entitlement),
    Duplicate(Entitlement),
    SubjectUnknown,
    SubjectMismatch,
    VersionConflict,
}

fn same_content(e: &Entitlement, i: &EntitlementInput) -> bool {
    e.subject_kind == i.subject_kind
        && e.subject_id == i.subject_id
        && e.plan_code == i.plan_code
        && e.plan_version == i.plan_version
        && e.names == i.names
        && e.seats == i.seats
        && e.agent_limit == i.agent_limit
        && e.storage_bytes == i.storage_bytes
        && e.status == i.status
        && e.period_end == i.period_end
        && e.paid_through == i.paid_through
        && e.cancel_requested_at == i.cancel_requested_at
        && e.pending_plan_code == i.pending_plan_code
        && e.pending_effective_at == i.pending_effective_at
}

fn write(c: &Connection, i: &EntitlementInput, version: i64, now: u64) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO entitlements (id,subject_kind,subject_id,source,plan_code,plan_version,names,seats,agent_limit,storage_bytes,
            status,period_end,paid_through,provider_ref,provider_environment,cancel_requested_at,pending_plan_code,pending_effective_at,
            updated_at,reconciliation_version)
         VALUES ('ent_'||lower(hex(randomblob(12))),?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
         ON CONFLICT(source,provider_environment,provider_ref) DO UPDATE SET
            subject_kind=excluded.subject_kind,subject_id=excluded.subject_id,plan_code=excluded.plan_code,
            plan_version=excluded.plan_version,names=excluded.names,seats=excluded.seats,agent_limit=excluded.agent_limit,
            storage_bytes=excluded.storage_bytes,status=excluded.status,period_end=excluded.period_end,
            paid_through=excluded.paid_through,cancel_requested_at=excluded.cancel_requested_at,
            pending_plan_code=excluded.pending_plan_code,pending_effective_at=excluded.pending_effective_at,
            updated_at=excluded.updated_at,reconciliation_version=excluded.reconciliation_version",
        params![
            i.subject_kind, i.subject_id, i.source, i.plan_code, i.plan_version, i.names, i.seats,
            i.agent_limit, i.storage_bytes, i.status, i.period_end, i.paid_through, i.reference,
            i.environment, i.cancel_requested_at, i.pending_plan_code, i.pending_effective_at,
            now as i64, version
        ],
    )?;
    Ok(())
}

/// The internal-API upsert. Runs inside the caller's write transaction.
pub(super) fn upsert(
    c: &Connection,
    i: &EntitlementInput,
    now: u64,
) -> Result<UpsertOutcome, StoreError> {
    let exists: bool = match i.subject_kind.as_str() {
        "account" => {
            let sub: Option<Option<String>> = c
                .query_row(
                    "SELECT oidc_sub FROM accounts WHERE id=?1",
                    [&i.subject_id],
                    |r| r.get(0),
                )
                .optional()?;
            match sub {
                None => false,
                Some(sub) => {
                    if let Some(expected) = &i.expected_oidc_sub {
                        if sub.as_deref() != Some(expected.as_str()) {
                            return Ok(UpsertOutcome::SubjectMismatch);
                        }
                    }
                    true
                }
            }
        }
        "org" => c.query_row(
            "SELECT EXISTS(SELECT 1 FROM orgs WHERE id=?1)",
            [&i.subject_id],
            |r| r.get(0),
        )?,
        _ => false,
    };
    if !exists {
        return Ok(UpsertOutcome::SubjectUnknown);
    }
    let previous = lookup(c, &i.source, &i.environment, &i.reference, now)?;
    if let Some(prev) = &previous {
        if prev.subject_kind != i.subject_kind || prev.subject_id != i.subject_id {
            return Ok(UpsertOutcome::SubjectMismatch);
        }
        if i.reconciliation_version < prev.reconciliation_version {
            return Ok(UpsertOutcome::Stale(prev.clone()));
        }
        if i.reconciliation_version == prev.reconciliation_version {
            if same_content(prev, i) {
                if i.subject_kind == "account" {
                    recompute_web(c, &i.subject_id, now)?;
                }
                return Ok(UpsertOutcome::Duplicate(prev.clone()));
            }
            return Ok(UpsertOutcome::VersionConflict);
        }
    }
    write(c, i, i.reconciliation_version, now)?;
    if i.subject_kind == "account" {
        recompute_web(c, &i.subject_id, now)?;
    } else {
        super::orgs::follow_entitlement(c, &i.subject_id, now)?;
    }
    let saved = lookup(c, &i.source, &i.environment, &i.reference, now)?
        .ok_or(StoreError::Corrupt("entitlement disappeared"))?;
    Ok(UpsertOutcome::Applied(saved))
}

// ---- store-derived rows ------------------------------------------------------------------------

/// Write a row the postbox derives from its own provider tables. The provider table is the
/// authority, so the subject follows it; the version only moves when the content changes.
fn derive(c: &Connection, mut i: EntitlementInput, now: u64) -> Result<(), StoreError> {
    let previous = lookup(c, &i.source, &i.environment, &i.reference, now)?;
    if let Some(prev) = &previous {
        if same_content(prev, &i) {
            return Ok(());
        }
        i.reconciliation_version = i
            .reconciliation_version
            .max(prev.reconciliation_version + 1);
    }
    let version = i.reconciliation_version;
    write(c, &i, version, now)
}

/// A provider product id as a plan code: lower-case, `[a-z0-9.-]`, at most 64 characters.
pub(crate) fn plan_code_for(product: &str) -> String {
    let mut code: String = product
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if !code.starts_with(|ch: char| ch.is_ascii_alphanumeric()) {
        code.insert(0, 'x');
        code.truncate(64);
    }
    code
}

fn apple_env(environment: &str) -> String {
    match environment.to_ascii_lowercase().as_str() {
        "production" => "live".into(),
        "sandbox" | "xcode" | "localtesting" => "sandbox".into(),
        other => plan_code_for(other),
    }
}

#[allow(clippy::too_many_arguments)]
fn derived(
    source: &str,
    env: String,
    reference: String,
    account: String,
    plan_code: String,
    plan_version: &str,
    names: i64,
    status: &str,
    paid_through: Option<i64>,
    now: u64,
) -> EntitlementInput {
    EntitlementInput {
        source: source.into(),
        environment: env,
        reference,
        subject_kind: "account".into(),
        subject_id: account,
        plan_code,
        plan_version: plan_version.into(),
        names: Some(names),
        seats: None,
        agent_limit: None,
        storage_bytes: None,
        status: status.into(),
        period_end: paid_through,
        paid_through,
        cancel_requested_at: None,
        pending_plan_code: None,
        pending_effective_at: None,
        reconciliation_version: (now as i64) * 1000,
        expected_oidc_sub: None,
    }
}

/// Mirror an account's Apple capacity plan.
pub(super) fn sync_apple_plan(c: &Connection, account: &str, now: u64) -> Result<(), StoreError> {
    let row: Option<(String, String, String, i64, bool)> = c
        .query_row(
            "SELECT original_transaction_id,product_id,environment,expires_at,active FROM apple_handle_plans WHERE account_id=?1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((original, product, env, expires, active)) = row else {
        return Ok(());
    };
    let names = crate::appstore::plan_capacity(&product).unwrap_or(0) as i64;
    derive(
        c,
        derived(
            "apple",
            apple_env(&env),
            original,
            account.into(),
            plan_code_for(&product),
            "apple",
            names,
            if active { "active" } else { "expired" },
            Some(expires),
            now,
        ),
        now,
    )
}

/// Mirror one legacy per-name Apple subscription.
pub(super) fn sync_apple_legacy(
    c: &Connection,
    original: &str,
    now: u64,
) -> Result<(), StoreError> {
    let row: Option<(String, String, String, i64)> = c
        .query_row(
            "SELECT account_id,environment,product_id,expires_at FROM apple_subscriptions WHERE original_transaction_id=?1",
            [original],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((account, env, product, expires)) = row else {
        return Ok(());
    };
    let product = if product.is_empty() {
        "dev.pigeonpost.inbox.handle.yearly".to_string()
    } else {
        product
    };
    derive(
        c,
        derived(
            "apple",
            apple_env(&env),
            original.into(),
            account,
            plan_code_for(&product),
            "apple-legacy",
            1,
            "active",
            Some(expires),
            now,
        ),
        now,
    )
}

/// Mirror one Google purchase token.
pub(super) fn sync_google(c: &Connection, token: &str, now: u64) -> Result<(), StoreError> {
    let row: Option<(String, String, i64, String, bool, Option<String>)> = c
        .query_row(
            "SELECT account_id,product_id,expires_at,state,test_purchase,replaced_by FROM google_subscriptions WHERE purchase_token=?1",
            [token],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let Some((account, product, expires, state, test, replaced)) = row else {
        return Ok(());
    };
    let status = if replaced.is_some() {
        "expired"
    } else {
        match state.as_str() {
            "SUBSCRIPTION_STATE_ACTIVE" => "active",
            "SUBSCRIPTION_STATE_IN_GRACE_PERIOD" => "grace",
            "SUBSCRIPTION_STATE_CANCELED" => "canceled",
            _ => "expired",
        }
    };
    derive(
        c,
        derived(
            "google",
            if test { "test".into() } else { "live".into() },
            token.into(),
            account,
            plan_code_for(&product),
            "google",
            1,
            status,
            Some(expires),
            now,
        ),
        now,
    )
}

/// Mirror one legacy namespace grant (`PUT /v1/namespaces`, namespaces.source='entitlement').
/// A grant whose namespace left that binding is marked expired, never extended.
pub(super) fn sync_grant(c: &Connection, namespace: &str, now: u64) -> Result<(), StoreError> {
    let reference = format!("namespace:{namespace}");
    let row: Option<(String, Option<i64>)> = c
        .query_row(
            "SELECT account_id,expires_at FROM namespaces WHERE namespace=?1 AND source='entitlement'",
            [namespace],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((account, expires)) => derive(
            c,
            derived(
                "grant",
                "live".into(),
                reference,
                account,
                LEGACY_GRANT_PLAN.into(),
                "legacy",
                1,
                "active",
                expires,
                now,
            ),
            now,
        ),
        None => {
            if let Some(prev) = lookup(c, "grant", "live", &reference, now)? {
                if prev.status != "expired" {
                    let mut input = derived(
                        "grant",
                        "live".into(),
                        reference,
                        prev.subject_id.clone(),
                        prev.plan_code.clone(),
                        "legacy",
                        1,
                        "expired",
                        Some(prev.paid_through.unwrap_or(now as i64).min(now as i64)),
                        now,
                    );
                    input.period_end = prev.period_end;
                    derive(c, input, now)?;
                }
            }
            Ok(())
        }
    }
}

/// Re-derive every mirrored row. Idempotent and cheap at current volume; never touches
/// `namespaces` or the provider tables.
pub(super) fn backfill(c: &Connection, now: u64) -> Result<(), StoreError> {
    let collect = |sql: &str| -> Result<Vec<String>, StoreError> {
        let mut stmt = c.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    for account in collect("SELECT account_id FROM apple_handle_plans")? {
        sync_apple_plan(c, &account, now)?;
    }
    for original in collect("SELECT original_transaction_id FROM apple_subscriptions")? {
        sync_apple_legacy(c, &original, now)?;
    }
    for token in collect("SELECT purchase_token FROM google_subscriptions")? {
        sync_google(c, &token, now)?;
    }
    for namespace in collect(
        "SELECT namespace FROM namespaces WHERE source='entitlement'
         UNION SELECT substr(provider_ref,11) FROM entitlements WHERE source='grant' AND provider_environment='live' AND provider_ref LIKE 'namespace:%'",
    )? {
        sync_grant(c, &namespace, now)?;
    }
    Ok(())
}

// ---- web plan names ----------------------------------------------------------------------------

/// The capacity-giving entitlements of an account (never org rows, never store rows).
const CAPACITY: &str = "subject_kind='account' AND subject_id=?1 AND source IN ('iyzico','grant','test') AND plan_code!='legacy-namespace-grant'";

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct WebPlan {
    pub capacity: i64,
    pub used: i64,
    pub active: bool,
    pub paid_through: Option<i64>,
    pub names: Vec<WebPlanName>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WebPlanName {
    pub slot: i64,
    pub namespace: String,
    pub expires_at: Option<i64>,
    pub active: bool,
}

type CapacityRow = (
    Option<i64>,
    Option<bool>,
    Option<bool>,
    Option<i64>,
    Option<i64>,
);

struct Capacity {
    capacity: i64,
    active: bool,
    perpetual: bool,
    max_active_paid: Option<i64>,
    max_paid: Option<i64>,
}

fn capacity(c: &Connection, account: &str, now: u64) -> Result<Capacity, StoreError> {
    let (cap, active, perpetual, max_active_paid, max_paid): CapacityRow = c.query_row(
        &format!(
            "SELECT MAX(CASE WHEN {LIVE} THEN names END), MAX({LIVE}), MAX(({LIVE}) AND paid_through IS NULL),
                    MAX(CASE WHEN {LIVE} THEN paid_through END), MAX(paid_through)
               FROM entitlements WHERE {CAPACITY}"
        ),
        params![account, now as i64],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let active = active.unwrap_or(false);
    Ok(Capacity {
        capacity: if active {
            cap.unwrap_or(0).clamp(0, MAX_WEB_NAMES)
        } else {
            0
        },
        active,
        perpetual: perpetual.unwrap_or(false),
        max_active_paid,
        max_paid,
    })
}

fn used(c: &Connection, account: &str) -> Result<i64, StoreError> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM plan_names s JOIN namespaces n ON n.namespace=s.namespace AND n.account_id=s.account_id
            AND n.source='web' AND n.provider_ref=?2 WHERE s.account_id=?1",
        params![account, format!("web:{account}")],
        |r| r.get(0),
    )?)
}

/// Bring the account's web-plan namespaces in line with its entitlements. A downgrade keeps the
/// first N slots; the rest, and everything after the plan lapses, enter the recovery period.
pub(super) fn recompute_web(c: &Connection, account: &str, now: u64) -> Result<(), StoreError> {
    let cap = capacity(c, account, now)?;
    let provider = format!("web:{account}");
    let now = now as i64;
    let recovery = lifecycle::RECOVERY_SECONDS;
    let slot = "(SELECT slot FROM plan_names s WHERE s.account_id=?1 AND s.namespace=namespaces.namespace)";
    let scope = "account_id=?1 AND source='web' AND provider_ref=?2
        AND EXISTS(SELECT 1 FROM plan_names s WHERE s.account_id=?1 AND s.namespace=namespaces.namespace)";
    if cap.active {
        let expiry = if cap.perpetual {
            None
        } else {
            cap.max_active_paid
        };
        c.execute(
            &format!(
                "UPDATE namespaces SET
                    expires_at=CASE WHEN {slot}<=?3 THEN ?4 ELSE MIN(COALESCE(expires_at,?5),?5) END,
                    release_at=CASE WHEN {slot}<=?3 THEN NULL ELSE COALESCE(release_at,?5+?6) END,
                    verified_at=?5
                 WHERE {scope}"
            ),
            params![account, provider, cap.capacity, expiry, now, recovery],
        )?;
    } else {
        let last = cap.max_paid.unwrap_or(now).min(now);
        c.execute(
            &format!(
                "UPDATE namespaces SET
                    expires_at=MIN(COALESCE(expires_at,?3),?3),
                    release_at=COALESCE(release_at,MIN(COALESCE(expires_at,?3),?3)+?5),
                    verified_at=?4
                 WHERE {scope}"
            ),
            params![account, provider, last, now, recovery],
        )?;
    }
    Ok(())
}

fn web_plan(c: &Connection, account: &str, now: u64) -> Result<WebPlan, StoreError> {
    let cap = capacity(c, account, now)?;
    let mut stmt = c.prepare(
        "SELECT s.slot,s.namespace,n.expires_at FROM plan_names s JOIN namespaces n ON n.namespace=s.namespace
            AND n.account_id=s.account_id AND n.source='web' AND n.provider_ref=?2
         WHERE s.account_id=?1 ORDER BY s.slot",
    )?;
    let names = stmt
        .query_map(params![account, format!("web:{account}")], |r| {
            let expires_at: Option<i64> = r.get(2)?;
            Ok(WebPlanName {
                slot: r.get(0)?,
                namespace: format!("/{}", r.get::<_, String>(1)?),
                expires_at,
                active: expires_at.is_none_or(|e| e > now as i64),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WebPlan {
        capacity: cap.capacity,
        used: names.len() as i64,
        active: cap.active,
        paid_through: if cap.perpetual {
            None
        } else {
            cap.max_active_paid.or(cap.max_paid)
        },
        names,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebClaim {
    Granted { capacity: i64, used: i64 },
    AlreadyInPlan { capacity: i64, used: i64 },
    NoPlan,
    LimitReached,
    AlreadyHeld,
    Taken,
}

impl Store {
    /// Upsert one entitlement from the internal API.
    pub async fn upsert_entitlement(
        &self,
        input: EntitlementInput,
        now: u64,
    ) -> Result<UpsertOutcome, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let outcome = upsert(&tx, &input, now)?;
            tx.commit()?;
            Ok(outcome)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn entitlements_for(
        &self,
        kind: &'static str,
        id: String,
        now: u64,
    ) -> Result<Vec<Entitlement>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            for_subject(&conn.lock().expect("store lock"), kind, &id, now)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn web_plan(&self, account: String, now: u64) -> Result<WebPlan, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            web_plan(&conn.lock().expect("store lock"), &account, now)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn account_exists(&self, account: String) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            Ok(conn.lock().expect("store lock").query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
                [account],
                |r| r.get(0),
            )?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Re-derive every store-mirrored entitlement (also run at open).
    #[cfg(test)]
    pub async fn backfill_entitlements(&self, now: u64) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            backfill(&tx, now)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Register a name in the account's web plan. Checks capacity, ownership and availability in
    /// one write transaction so concurrent claims cannot overfill the plan.
    pub async fn claim_web_name(
        &self,
        account: String,
        name: String,
        now: u64,
    ) -> Result<WebClaim, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let cap = capacity(&tx, &account, now)?;
            if !cap.active || cap.capacity == 0 {
                return Ok(WebClaim::NoPlan);
            }
            let provider = format!("web:{account}");
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT s.slot FROM plan_names s JOIN namespaces n ON n.namespace=s.namespace
                        AND n.account_id=s.account_id AND n.source='web' AND n.provider_ref=?3
                     WHERE s.account_id=?1 AND s.namespace=?2",
                    params![account, name, provider],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(slot) = existing {
                let used = used(&tx, &account)?;
                return Ok(if slot <= cap.capacity {
                    WebClaim::AlreadyInPlan {
                        capacity: cap.capacity,
                        used,
                    }
                } else {
                    WebClaim::LimitReached
                });
            }
            let held: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE namespace=?1 AND account_id=?2 AND (expires_at IS NULL OR expires_at>?3))",
                params![name, account, now as i64],
                |r| r.get(0),
            )?;
            if held {
                return Ok(WebClaim::AlreadyHeld);
            }
            if !lifecycle::available_to(&tx, &name, Some(&account), now)? {
                return Ok(WebClaim::Taken);
            }
            let mut slot = None;
            for candidate in 1..=cap.capacity {
                let occupied: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM plan_names s JOIN namespaces n ON n.namespace=s.namespace
                        AND n.account_id=s.account_id AND n.source='web' AND n.provider_ref=?3
                     WHERE s.account_id=?1 AND s.slot=?2)",
                    params![account, candidate, provider],
                    |r| r.get(0),
                )?;
                if !occupied {
                    slot = Some(candidate);
                    break;
                }
            }
            let Some(slot) = slot else {
                return Ok(WebClaim::LimitReached);
            };
            let expiry = if cap.perpetual {
                None
            } else {
                cap.max_active_paid
            };
            lifecycle::detach_previous_aliases(&tx, &name, &account)?;
            tx.execute(
                "DELETE FROM plan_names WHERE account_id=?1 AND (slot=?2 OR namespace=?3)",
                params![account, slot, name],
            )?;
            tx.execute(
                "INSERT INTO plan_names(account_id,slot,namespace) VALUES(?1,?2,?3)",
                params![account, slot, name],
            )?;
            tx.execute(
                "INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at,provider_ref,release_at)
                 VALUES(?1,?2,'web',?3,?4,?5,NULL) ON CONFLICT(namespace) DO UPDATE SET
                 account_id=?2,source='web',verified_at=?3,expires_at=?4,provider_ref=?5,release_at=NULL",
                params![name, account, now as i64, expiry, provider],
            )?;
            let used = used(&tx, &account)?;
            tx.commit()?;
            Ok(WebClaim::Granted {
                capacity: cap.capacity,
                used,
            })
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The legacy tier rule, kept for the parity test only.
    #[cfg(test)]
    pub async fn legacy_paid(&self, account: String, now: u64) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            Ok(conn.lock().expect("store lock").query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces
                    WHERE account_id = ?1 AND (expires_at IS NULL OR expires_at > ?2))
                 OR EXISTS(SELECT 1 FROM apple_handle_plans
                    WHERE account_id = ?1 AND active = 1 AND expires_at > ?2)",
                params![account, now as i64],
                |r| r.get(0),
            )?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// The unified rule `tier_of` uses for an account.
    #[cfg(test)]
    pub async fn account_paid(&self, account: String, now: u64) -> Result<bool, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || paid(&conn.lock().expect("store lock"), &account, now))
            .await
            .map_err(|_| StoreError::Join)?
    }
}

/// Paid hosting: an active namespace (unchanged), or any active account entitlement. Org rows
/// never make a person paid.
pub(super) fn paid(c: &Connection, account: &str, now: u64) -> Result<bool, StoreError> {
    Ok(c.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM namespaces
                WHERE account_id = ?1 AND (expires_at IS NULL OR expires_at > ?2))
             OR EXISTS(SELECT 1 FROM entitlements
                WHERE subject_kind='account' AND subject_id=?1 AND {LIVE})"
        ),
        params![account, now as i64],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstore::{account_token, Entitlement as AppleEntitlement, SubscriptionStatus};
    use crate::googleplay::VerifiedPurchase;

    const YEAR: i64 = 365 * 86400;

    fn exec(s: &Store, sql: &str) {
        s.conn.lock().unwrap().execute_batch(sql).unwrap();
    }
    fn account(s: &Store, id: &str, sub: Option<&str>) {
        s.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO accounts(id,created_at,oidc_sub) VALUES(?1,1,?2)",
                params![id, sub],
            )
            .unwrap();
    }
    fn web(
        account: &str,
        reference: &str,
        names: i64,
        paid: Option<i64>,
        v: i64,
    ) -> EntitlementInput {
        EntitlementInput {
            source: "iyzico".into(),
            environment: "live".into(),
            reference: reference.into(),
            subject_kind: "account".into(),
            subject_id: account.into(),
            plan_code: format!("cloud-{names}names-yearly-usd"),
            plan_version: "2026".into(),
            names: Some(names),
            seats: None,
            agent_limit: None,
            storage_bytes: None,
            status: "active".into(),
            period_end: paid,
            paid_through: paid,
            cancel_requested_at: None,
            pending_plan_code: None,
            pending_effective_at: None,
            reconciliation_version: v,
            expected_oidc_sub: None,
        }
    }
    fn apple_status(account: &str, n: usize, expires: i64, active: bool) -> SubscriptionStatus {
        SubscriptionStatus {
            entitlement: AppleEntitlement {
                original_transaction_id: format!("plan-{account}"),
                product_id: format!("dev.pigeonpost.inbox.handles.{n}.yearly"),
                app_account_token: Some(account_token(account)),
                expires_at: expires,
                environment: "Production".into(),
            },
            active,
            terminal: !active,
            revoked: false,
        }
    }
    fn google(expires: u64, state: &str, active: bool) -> VerifiedPurchase {
        VerifiedPurchase {
            product_id: "pigeonpost.handle.01".into(),
            linked_token: None,
            expires_at: expires,
            state: state.into(),
            active,
            auto_renewing: true,
            acknowledged: true,
            test_purchase: false,
        }
    }

    async fn assert_parity(s: &Store, accounts: &[&str], times: &[u64]) {
        for &now in times {
            for a in accounts {
                assert_eq!(
                    s.legacy_paid(a.to_string(), now).await.unwrap(),
                    s.account_paid(a.to_string(), now).await.unwrap(),
                    "tier parity for {a} at {now}"
                );
            }
        }
    }

    const ACCOUNTS: &[&str] = &[
        "plan_before_name",
        "plan_named",
        "plan_lapsed",
        "legacy_apple",
        "google_live",
        "google_done",
        "grant_forever",
        "grant_dated",
        "preview",
        "nobody",
    ];
    const TIMES: &[u64] = &[
        150,
        1_000 + YEAR as u64 - 1,
        1_000 + YEAR as u64 + 1,
        10 * YEAR as u64,
    ];

    #[tokio::test]
    async fn the_unified_tier_matches_the_legacy_rule_for_every_provider_path() {
        let s = Store::open(":memory:").unwrap();
        for a in ACCOUNTS {
            account(&s, a, None);
        }
        let until = 1_000 + YEAR;
        s.apply_apple_plan(
            "plan_before_name".into(),
            apple_status("plan_before_name", 2, until, true),
            None,
            100,
        )
        .await
        .unwrap();
        s.apply_apple_plan(
            "plan_named".into(),
            apple_status("plan_named", 1, until, true),
            Some("named".into()),
            100,
        )
        .await
        .unwrap();
        s.apply_apple_plan(
            "plan_lapsed".into(),
            apple_status("plan_lapsed", 1, until, true),
            Some("lapsing".into()),
            100,
        )
        .await
        .unwrap();
        s.apply_apple_plan(
            "plan_lapsed".into(),
            apple_status("plan_lapsed", 1, 120, false),
            None,
            130,
        )
        .await
        .unwrap();
        s.bind_apple_subscription(
            crate::store::ApplePurchase {
                original_transaction_id: "legacy-1".into(),
                product_id: "dev.pigeonpost.inbox.handle.yearly".into(),
                environment: "Production".into(),
                expires_at: until,
            },
            "legacy_apple".into(),
            "oldname".into(),
            100,
        )
        .await
        .unwrap();
        s.record_google_purchase(
            "google_live".into(),
            "tok-live".into(),
            google(until as u64, "SUBSCRIPTION_STATE_ACTIVE", true),
            Some("droid".into()),
            100,
        )
        .await
        .unwrap();
        s.record_google_purchase(
            "google_done".into(),
            "tok-done".into(),
            google(until as u64, "SUBSCRIPTION_STATE_ACTIVE", true),
            Some("gone".into()),
            100,
        )
        .await
        .unwrap();
        s.record_google_purchase(
            "google_done".into(),
            "tok-done".into(),
            google(140, "SUBSCRIPTION_STATE_EXPIRED", false),
            None,
            140,
        )
        .await
        .unwrap();
        s.set_namespace_owner(
            "forever".into(),
            "grant_forever".into(),
            "entitlement",
            100,
            None,
        )
        .await
        .unwrap();
        s.set_namespace_owner(
            "dated".into(),
            "grant_dated".into(),
            "entitlement",
            100,
            Some(until as u64),
        )
        .await
        .unwrap();
        exec(&s, "INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at) VALUES('peek','preview','test_preview',100,NULL)");
        assert_parity(&s, ACCOUNTS, TIMES).await;

        // Every provider path mirrored itself in the same transaction.
        let mirrored = |a: &'static str| {
            let s = &s;
            async move { s.entitlements_for("account", a.into(), 150).await.unwrap() }
        };
        assert_eq!(mirrored("plan_before_name").await[0].names, Some(2));
        assert_eq!(mirrored("plan_lapsed").await[0].status, "expired");
        assert_eq!(
            mirrored("legacy_apple").await[0].plan_version,
            "apple-legacy"
        );
        assert_eq!(mirrored("google_done").await[0].status, "expired");
        assert_eq!(mirrored("grant_forever").await[0].paid_through, None);
        assert!(mirrored("preview").await.is_empty());

        // The one intentional difference: a paid Google slot whose requested name was taken now
        // gives hosted storage, as an Apple plan without a name already did.
        account(&s, "google_unnamed", None);
        s.record_google_purchase(
            "google_unnamed".into(),
            "tok-unnamed".into(),
            google(until as u64, "SUBSCRIPTION_STATE_ACTIVE", true),
            Some("droid".into()),
            100,
        )
        .await
        .unwrap();
        assert!(!s.legacy_paid("google_unnamed".into(), 150).await.unwrap());
        assert!(s.account_paid("google_unnamed".into(), 150).await.unwrap());

        // Backfill is idempotent: no version moves on a second pass.
        let before: Vec<_> = futures_versions(&s).await;
        s.backfill_entitlements(10_000).await.unwrap();
        assert_eq!(before, futures_versions(&s).await);
        assert_parity(&s, ACCOUNTS, TIMES).await;
    }

    async fn futures_versions(s: &Store) -> Vec<(String, i64)> {
        let c = s.conn.lock().unwrap();
        let mut stmt = c.prepare("SELECT provider_ref,reconciliation_version FROM entitlements ORDER BY provider_ref").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn dump(s: &Store, table: &str) -> String {
        let c = s.conn.lock().unwrap();
        let mut stmt = c
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let cols = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..cols)
                    .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.join("\n")
    }

    /// Rows written by an older binary (no mirror) are backfilled without touching access.
    #[tokio::test]
    async fn backfill_mirrors_legacy_rows_and_changes_nothing_else() {
        let s = Store::open(":memory:").unwrap();
        exec(&s, "
            INSERT INTO accounts(id,created_at) VALUES('a',1),('b',1),('c',1),('d',1);
            INSERT INTO apple_handle_plans VALUES('a','orig-a','dev.pigeonpost.inbox.handles.3.yearly','Sandbox',5000,1,100);
            INSERT INTO apple_subscriptions(original_transaction_id,account_id,namespace,environment,product_id,expires_at,created_at,updated_at)
                VALUES('orig-b','b','bee','Production','',5000,100,100);
            INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at,provider_ref) VALUES('bee','b','apple',100,5000,'orig-b');
            INSERT INTO google_subscriptions VALUES('tok-c','c','pigeonpost.handle.01','sea',5000,'SUBSCRIPTION_STATE_IN_GRACE_PERIOD',1,1,1,100,NULL);
            INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at,provider_ref) VALUES('sea','c','google',100,5000,'tok-c');
            INSERT INTO namespaces(namespace,account_id,source,verified_at,expires_at) VALUES('dee','d','entitlement',100,NULL);
        ");
        let before: Vec<String> = [
            "namespaces",
            "apple_handle_plans",
            "apple_subscriptions",
            "google_subscriptions",
        ]
        .iter()
        .map(|t| dump(&s, t))
        .collect();
        assert_ne!(
            s.legacy_paid("a".into(), 200).await.unwrap(),
            s.account_paid("a".into(), 200).await.unwrap(),
            "precondition: an unmirrored plan is invisible to the unified rule"
        );
        s.backfill_entitlements(200).await.unwrap();
        let after: Vec<String> = [
            "namespaces",
            "apple_handle_plans",
            "apple_subscriptions",
            "google_subscriptions",
        ]
        .iter()
        .map(|t| dump(&s, t))
        .collect();
        assert_eq!(
            before, after,
            "backfill must not touch provider tables or namespaces"
        );
        assert_parity(&s, &["a", "b", "c", "d"], &[200, 4999, 5001]).await;
        let a = s
            .entitlements_for("account", "a".into(), 200)
            .await
            .unwrap();
        assert_eq!(
            (
                a[0].source.as_str(),
                a[0].provider_environment.as_str(),
                a[0].names
            ),
            ("apple", "sandbox", Some(3))
        );
        let c = s
            .entitlements_for("account", "c".into(), 200)
            .await
            .unwrap();
        assert_eq!(
            (c[0].status.as_str(), c[0].provider_environment.as_str()),
            ("grace", "test")
        );
        let d = s
            .entitlements_for("account", "d".into(), 200)
            .await
            .unwrap();
        assert_eq!(
            (d[0].plan_code.as_str(), d[0].provider_ref.as_str()),
            (LEGACY_GRANT_PLAN, "namespace:dee")
        );
        // A legacy grant never adds web plan capacity.
        assert_eq!(s.web_plan("d".into(), 200).await.unwrap().capacity, 0);
        assert_eq!(
            s.claim_web_name("d".into(), "extra".into(), 200)
                .await
                .unwrap(),
            WebClaim::NoPlan
        );
    }

    #[tokio::test]
    async fn versions_are_monotonic_and_subjects_are_bound() {
        let s = Store::open(":memory:").unwrap();
        account(&s, "acct", Some("sub-1"));
        account(&s, "other", None);
        let first = web("acct", "sub-a", 3, Some(5000), 10);
        assert!(matches!(
            s.upsert_entitlement(first.clone(), 100).await.unwrap(),
            UpsertOutcome::Applied(_)
        ));
        assert!(matches!(
            s.upsert_entitlement(first.clone(), 100).await.unwrap(),
            UpsertOutcome::Duplicate(_)
        ));
        let mut changed = first.clone();
        changed.names = Some(4);
        assert_eq!(
            s.upsert_entitlement(changed.clone(), 100).await.unwrap(),
            UpsertOutcome::VersionConflict
        );
        let mut older = changed.clone();
        older.reconciliation_version = 9;
        match s.upsert_entitlement(older, 100).await.unwrap() {
            UpsertOutcome::Stale(e) => assert_eq!(e.names, Some(3)),
            other => panic!("{other:?}"),
        }
        changed.reconciliation_version = 11;
        assert!(
            matches!(s.upsert_entitlement(changed, 100).await.unwrap(), UpsertOutcome::Applied(e) if e.names == Some(4))
        );
        let mut stolen = web("other", "sub-a", 3, Some(5000), 12);
        assert_eq!(
            s.upsert_entitlement(stolen.clone(), 100).await.unwrap(),
            UpsertOutcome::SubjectMismatch
        );
        stolen.subject_id = "ghost".into();
        stolen.reference = "sub-b".into();
        assert_eq!(
            s.upsert_entitlement(stolen, 100).await.unwrap(),
            UpsertOutcome::SubjectUnknown
        );
        let mut checked = web("acct", "sub-c", 1, Some(5000), 1);
        checked.expected_oidc_sub = Some("sub-2".into());
        assert_eq!(
            s.upsert_entitlement(checked.clone(), 100).await.unwrap(),
            UpsertOutcome::SubjectMismatch
        );
        checked.expected_oidc_sub = Some("sub-1".into());
        assert!(matches!(
            s.upsert_entitlement(checked, 100).await.unwrap(),
            UpsertOutcome::Applied(_)
        ));
        let mut org = web("acct", "sub-d", 1, Some(5000), 1);
        org.subject_kind = "org".into();
        org.subject_id = "org_missing".into();
        assert_eq!(
            s.upsert_entitlement(org, 100).await.unwrap(),
            UpsertOutcome::SubjectUnknown
        );
    }

    #[tokio::test]
    async fn capacity_is_the_maximum_not_the_sum_and_downgrades_recover_extra_names() {
        let s = Store::open(":memory:").unwrap();
        account(&s, "acct", None);
        assert_eq!(
            s.claim_web_name("acct".into(), "one".into(), 100)
                .await
                .unwrap(),
            WebClaim::NoPlan
        );
        s.upsert_entitlement(web("acct", "sub-a", 2, Some(5000), 1), 100)
            .await
            .unwrap();
        s.upsert_entitlement(web("acct", "sub-b", 2, Some(6000), 1), 100)
            .await
            .unwrap();
        assert_eq!(
            s.web_plan("acct".into(), 100).await.unwrap().capacity,
            2,
            "two 2-name plans give 2, not 4"
        );
        assert_eq!(
            s.claim_web_name("acct".into(), "one".into(), 100)
                .await
                .unwrap(),
            WebClaim::Granted {
                capacity: 2,
                used: 1
            }
        );
        assert_eq!(
            s.claim_web_name("acct".into(), "one".into(), 100)
                .await
                .unwrap(),
            WebClaim::AlreadyInPlan {
                capacity: 2,
                used: 1
            }
        );
        assert_eq!(
            s.claim_web_name("acct".into(), "two".into(), 100)
                .await
                .unwrap(),
            WebClaim::Granted {
                capacity: 2,
                used: 2
            }
        );
        assert_eq!(
            s.claim_web_name("acct".into(), "three".into(), 100)
                .await
                .unwrap(),
            WebClaim::LimitReached
        );
        let plan = s.web_plan("acct".into(), 100).await.unwrap();
        assert!(
            plan.names
                .iter()
                .all(|n| n.active && n.expires_at == Some(6000)),
            "names follow the latest paid_through"
        );
        assert!(s.account_paid("acct".into(), 100).await.unwrap());

        // Another account cannot take a held name.
        account(&s, "rival", None);
        s.upsert_entitlement(web("rival", "sub-r", 1, Some(5000), 1), 100)
            .await
            .unwrap();
        assert_eq!(
            s.claim_web_name("rival".into(), "one".into(), 100)
                .await
                .unwrap(),
            WebClaim::Taken
        );

        // Downgrade both to one name: slot 1 stays, slot 2 enters recovery.
        s.upsert_entitlement(web("acct", "sub-a", 1, Some(5000), 2), 200)
            .await
            .unwrap();
        s.upsert_entitlement(web("acct", "sub-b", 1, Some(6000), 2), 200)
            .await
            .unwrap();
        let plan = s.web_plan("acct".into(), 201).await.unwrap();
        assert_eq!(plan.capacity, 1);
        assert!(plan.names[0].active);
        assert!(!plan.names[1].active);
        assert!(
            !s.namespace_available("two".into(), 201).await.unwrap(),
            "recovery keeps the name reserved"
        );
        // Upgrade restores it.
        s.upsert_entitlement(web("acct", "sub-b", 2, Some(6000), 3), 300)
            .await
            .unwrap();
        assert!(s
            .web_plan("acct".into(), 301)
            .await
            .unwrap()
            .names
            .iter()
            .all(|n| n.active));

        // Lapse: everything expires at the last paid_through and is released after recovery.
        let mut lapsed = web("acct", "sub-a", 1, Some(5000), 3);
        lapsed.status = "canceled".into();
        s.upsert_entitlement(lapsed, 7000).await.unwrap();
        let mut lapsed = web("acct", "sub-b", 2, Some(6000), 4);
        lapsed.status = "expired".into();
        s.upsert_entitlement(lapsed, 7000).await.unwrap();
        assert!(!s.account_paid("acct".into(), 7000).await.unwrap());
        assert!(!s.legacy_paid("acct".into(), 7000).await.unwrap());
        let plan = s.web_plan("acct".into(), 7000).await.unwrap();
        assert!(
            !plan.active
                && plan
                    .names
                    .iter()
                    .all(|n| !n.active && n.expires_at == Some(6000))
        );
        let later = 6000 + lifecycle::RECOVERY_SECONDS as u64 + 1;
        // Fresh verification lets the name go after recovery.
        let mut refresh = web("acct", "sub-b", 2, Some(6000), 4);
        refresh.status = "expired".into();
        s.upsert_entitlement(refresh, later).await.unwrap();
        assert!(s.namespace_available("one".into(), later).await.unwrap());
    }

    #[tokio::test]
    async fn perpetual_grants_give_names_without_expiry_and_org_rows_give_nothing_personal() {
        let s = Store::open(":memory:").unwrap();
        account(&s, "acct", None);
        let mut grant = web("acct", "account:acct", 2, None, 1);
        grant.source = "grant".into();
        grant.plan_code = "cloud-2names-grant".into();
        s.upsert_entitlement(grant, 100).await.unwrap();
        assert!(matches!(
            s.claim_web_name("acct".into(), "free".into(), 100)
                .await
                .unwrap(),
            WebClaim::Granted { .. }
        ));
        assert_eq!(
            s.web_plan("acct".into(), u64::MAX / 4).await.unwrap().names[0].expires_at,
            None
        );

        account(&s, "owner", None);
        let draft = crate::store::OrgDraft {
            operation_id: "op_0123456789abcdef".into(),
            owner_account: "owner".into(),
            name: "Acme".into(),
            namespace: "acme".into(),
            plan_code: "ent-5-monthly-usd".into(),
            seats: 5,
        };
        let crate::store::DraftOutcome::Created(org) =
            s.create_org_draft(draft, 100).await.unwrap()
        else {
            panic!()
        };
        let mut ent = web(&org.id, "sub-org", 1, Some(5000), 1);
        ent.subject_kind = "org".into();
        ent.names = None;
        ent.seats = Some(5);
        ent.agent_limit = Some(50);
        assert!(matches!(
            s.upsert_entitlement(ent, 100).await.unwrap(),
            UpsertOutcome::Applied(_)
        ));
        assert!(
            !s.account_paid("owner".into(), 100).await.unwrap(),
            "org seats never make the owner personally paid"
        );
        assert_eq!(s.web_plan("owner".into(), 100).await.unwrap().capacity, 0);
    }
}
