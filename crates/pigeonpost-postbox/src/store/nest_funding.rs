//! Nest funding (Nests Phase B1): store purchases of Nest packages, purchase intents, the
//! allowance resolver and paid-but-unprovisioned setups.
//!
//! - `nest_store_purchases` is the record of provider state for App Store and Google Play Nest
//!   subscriptions (the handle tables are not reused: a Nest package is not a name). Every write
//!   mirrors into `entitlements` in the same transaction, plan code = the package code,
//!   plan version = the catalog edition. Web (iyzico) Nest entitlements arrive through the
//!   internal API from the billing bridge, like Cloud plans.
//! - The **allowance** of an account is decided from its live Nest entitlements only, never from
//!   the flag or a client: one funding package per account (the highest rank wins; any other live
//!   Nest package is reported as a duplicate for a refund, never added).
//! - `nest_setups` holds "paid, setup pending": a verified purchase whose first Nest has not been
//!   provisioned yet (or failed to). It survives restarts and is retried by the reconciler.
//! - `nests.funding` separates package-funded Nests (`package`) from Phase A ops-created ones
//!   (`ops`), which the funding sweep never touches.
use super::entitlements::{self, EntitlementInput};
use super::{params, Connection, OptionalExtension, StoreError};
use crate::nest_catalog::{self, Package};
use serde::Serialize;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nest_store_purchases (
    source       TEXT NOT NULL CHECK (source IN ('apple','google')),
    environment  TEXT NOT NULL CHECK (environment IN ('live','sandbox','test')),
    reference    TEXT NOT NULL,
    account_id   TEXT NOT NULL,
    product_id   TEXT NOT NULL,
    package      TEXT NOT NULL,
    expires_at   INTEGER NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('active','grace','canceled','expired','revoked','replaced','inactive')),
    acknowledged INTEGER NOT NULL DEFAULT 0,
    replaced_by  TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (source, environment, reference)
);
CREATE INDEX IF NOT EXISTS nest_store_purchases_by_account ON nest_store_purchases(account_id);
CREATE TABLE IF NOT EXISTS nest_purchase_intents (
    id              TEXT PRIMARY KEY,
    account_id      TEXT NOT NULL,
    package         TEXT NOT NULL,
    rail            TEXT NOT NULL CHECK (rail IN ('apple','google','web')),
    catalog_version TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    consumed_at     INTEGER,
    reference       TEXT
);
CREATE INDEX IF NOT EXISTS nest_purchase_intents_by_account ON nest_purchase_intents(account_id, created_at);
CREATE TABLE IF NOT EXISTS nest_setups (
    id              TEXT PRIMARY KEY,
    account_id      TEXT NOT NULL,
    entitlement_id  TEXT NOT NULL UNIQUE,
    label           TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('pending','done','failed','canceled')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    last_error      TEXT,
    nest_id         TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_setups_due ON nest_setups(status, next_attempt_at);
";

pub(super) const MIGRATIONS: &[&str] = &[
    "ALTER TABLE nests ADD COLUMN funding TEXT NOT NULL DEFAULT 'ops'",
    "ALTER TABLE nests ADD COLUMN paused_reason TEXT",
    "ALTER TABLE nests ADD COLUMN unfunded_since INTEGER",
];

/// A purchase intent lives this long; the store sheet is shown within it.
pub const INTENT_SECS: u64 = 3600;
/// After the funding of a Nest ends, its owner has this long to export before it is deleted
/// (hosted-agents plan §9.3: a disclosed 30-day window).
pub const EXPORT_WINDOW_SECS: i64 = 30 * 86400;
/// Setup retries back off to at most this.
pub const SETUP_MAX_BACKOFF_SECS: i64 = 3600;
/// After this many failed attempts a setup stays `failed` until the owner retries it.
pub const SETUP_MAX_ATTEMPTS: i64 = 8;

/// A provider-verified store subscription of a Nest package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePurchase {
    pub source: &'static str,
    pub environment: String,
    pub reference: String,
    pub product_id: String,
    pub expires_at: i64,
    /// One of the `state` values of `nest_store_purchases`.
    pub state: String,
    pub acknowledged: bool,
    /// Google: the purchase token this one replaced (upgrade/downgrade/resubscribe).
    pub replaces: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreClaim {
    /// Recorded (new or refreshed). `entitlement_id` is the mirrored entitlement.
    Saved {
        entitlement_id: String,
        package: &'static str,
        active: bool,
        /// Another live Nest package already funds this account: this one is a duplicate.
        duplicate: bool,
    },
    /// The subscription is already bound to a different Pigeonpost account.
    BoundElsewhere,
    /// The product is not a Nest package.
    UnknownProduct,
}

fn entitlement_status(state: &str) -> &'static str {
    match state {
        "active" => "active",
        "grace" => "grace",
        "canceled" => "canceled",
        _ => "expired",
    }
}

fn mirror(
    c: &Connection,
    account: &str,
    p: &StorePurchase,
    package: &Package,
    now: u64,
) -> Result<String, StoreError> {
    // Revoked and replaced subscriptions stop at once: their paid-through is cut to now.
    let paid_through = if matches!(p.state.as_str(), "revoked" | "replaced") {
        p.expires_at.min(now as i64)
    } else {
        p.expires_at
    };
    let input = EntitlementInput {
        source: p.source.into(),
        environment: p.environment.clone(),
        reference: p.reference.clone(),
        subject_kind: "account".into(),
        subject_id: account.into(),
        plan_code: package.code.into(),
        plan_version: nest_catalog::CATALOG_VERSION.into(),
        names: None,
        seats: None,
        agent_limit: None,
        storage_bytes: Some(package.storage_bytes),
        status: entitlement_status(&p.state).into(),
        period_end: Some(p.expires_at),
        paid_through: Some(paid_through),
        cancel_requested_at: None,
        pending_plan_code: None,
        pending_effective_at: None,
        reconciliation_version: (now as i64) * 1000,
        expected_oidc_sub: None,
    };
    entitlements::derive(c, input, now)?;
    let saved = entitlements::lookup(c, p.source, &p.environment, &p.reference, now)?
        .ok_or(StoreError::Corrupt("nest entitlement disappeared"))?;
    Ok(saved.id)
}

/// Record a verified store purchase for `account` and mirror it. Runs in the caller's write
/// transaction. A subscription is bound to the first account that proves it, for ever.
pub(crate) fn record_store_purchase(
    c: &Connection,
    account: &str,
    p: &StorePurchase,
    now: u64,
) -> Result<StoreClaim, StoreError> {
    let Some(package) = (match p.source {
        "apple" => nest_catalog::by_apple_product(&p.product_id),
        _ => nest_catalog::by_google_product(&p.product_id),
    }) else {
        return Ok(StoreClaim::UnknownProduct);
    };
    let bound: Option<String> = c
        .query_row(
            "SELECT account_id FROM nest_store_purchases WHERE source=?1 AND environment=?2 AND reference=?3",
            params![p.source, p.environment, p.reference],
            |r| r.get(0),
        )
        .optional()?;
    if bound.as_deref().is_some_and(|a| a != account) {
        return Ok(StoreClaim::BoundElsewhere);
    }
    // A replacement (Google upgrade/downgrade) retires the token it replaces, if it is ours.
    if let Some(old) = &p.replaces {
        let old_owner: Option<String> = c
            .query_row(
                "SELECT account_id FROM nest_store_purchases WHERE source=?1 AND environment=?2 AND reference=?3",
                params![p.source, p.environment, old],
                |r| r.get(0),
            )
            .optional()?;
        if old_owner.as_deref().is_some_and(|a| a != account) {
            return Ok(StoreClaim::BoundElsewhere);
        }
        if old_owner.is_some() {
            c.execute(
                "UPDATE nest_store_purchases SET state='replaced', replaced_by=?4, updated_at=?5
                 WHERE source=?1 AND environment=?2 AND reference=?3",
                params![p.source, p.environment, old, p.reference, now as i64],
            )?;
            let old_row = load_purchase(c, p.source, &p.environment, old)?
                .ok_or(StoreError::Corrupt("replaced purchase disappeared"))?;
            if let Some(old_pkg) = nest_catalog::by_code(&old_row.1) {
                mirror(c, account, &old_row.0, old_pkg, now)?;
            }
        }
    }
    // A purchase already marked replaced stays replaced, whatever a late verification says.
    let previous_state: Option<String> = c
        .query_row(
            "SELECT state FROM nest_store_purchases WHERE source=?1 AND environment=?2 AND reference=?3",
            params![p.source, p.environment, p.reference],
            |r| r.get(0),
        )
        .optional()?;
    let state = if previous_state.as_deref() == Some("replaced") {
        "replaced".to_string()
    } else {
        p.state.clone()
    };
    c.execute(
        "INSERT INTO nest_store_purchases (source,environment,reference,account_id,product_id,package,expires_at,state,acknowledged,created_at,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)
         ON CONFLICT(source,environment,reference) DO UPDATE SET
            product_id=excluded.product_id, package=excluded.package, expires_at=excluded.expires_at,
            state=excluded.state, acknowledged=MAX(acknowledged, excluded.acknowledged), updated_at=excluded.updated_at",
        params![
            p.source,
            p.environment,
            p.reference,
            account,
            p.product_id,
            package.code,
            p.expires_at,
            state,
            p.acknowledged,
            now as i64
        ],
    )?;
    let stored = StorePurchase {
        state: state.clone(),
        ..p.clone()
    };
    let entitlement_id = mirror(c, account, &stored, package, now)?;
    let active =
        matches!(state.as_str(), "active" | "grace" | "canceled") && p.expires_at > now as i64;
    let allowance = allowance(c, account, now)?;
    let duplicate = active
        && allowance
            .duplicates
            .iter()
            .any(|d| d.entitlement_id == entitlement_id);
    Ok(StoreClaim::Saved {
        entitlement_id,
        package: package.code,
        active,
        duplicate,
    })
}

type PurchaseRow = (StorePurchase, String);

fn load_purchase(
    c: &Connection,
    source: &str,
    env: &str,
    reference: &str,
) -> Result<Option<PurchaseRow>, StoreError> {
    Ok(c.query_row(
        "SELECT source,environment,reference,product_id,expires_at,state,acknowledged,package
               FROM nest_store_purchases WHERE source=?1 AND environment=?2 AND reference=?3",
        params![source, env, reference],
        |r| {
            let source: String = r.get(0)?;
            Ok((
                StorePurchase {
                    source: if source == "apple" { "apple" } else { "google" },
                    environment: r.get(1)?,
                    reference: r.get(2)?,
                    product_id: r.get(3)?,
                    expires_at: r.get(4)?,
                    state: r.get(5)?,
                    acknowledged: r.get(6)?,
                    replaces: None,
                },
                r.get(7)?,
            ))
        },
    )
    .optional()?)
}

/// Mark a Google purchase acknowledged (after Google confirmed it).
pub(crate) fn mark_acknowledged(c: &Connection, env: &str, token: &str) -> Result<(), StoreError> {
    c.execute(
        "UPDATE nest_store_purchases SET acknowledged=1 WHERE source='google' AND environment=?1 AND reference=?2",
        params![env, token],
    )?;
    Ok(())
}

/// One store purchase the reconciler should re-verify.
#[derive(Clone, Debug)]
pub struct DuePurchase {
    pub source: String,
    pub environment: String,
    pub reference: String,
    pub account_id: String,
    pub product_id: String,
    pub acknowledged: bool,
}

/// Store purchases whose state can still change on the provider's side: everything not
/// replaced, expired more than 60 days ago, or re-checked within `min_gap` seconds.
pub(crate) fn due_purchases(
    c: &Connection,
    now: u64,
    min_gap: i64,
) -> Result<Vec<DuePurchase>, StoreError> {
    let mut stmt = c.prepare(
        "SELECT source,environment,reference,account_id,product_id,acknowledged FROM nest_store_purchases
          WHERE state NOT IN ('replaced','revoked') AND expires_at > ?1 - 60*86400 AND updated_at < ?1 - ?2
          ORDER BY updated_at LIMIT 200",
    )?;
    let rows = stmt
        .query_map(params![now as i64, min_gap], |r| {
            Ok(DuePurchase {
                source: r.get(0)?,
                environment: r.get(1)?,
                reference: r.get(2)?,
                account_id: r.get(3)?,
                product_id: r.get(4)?,
                acknowledged: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Bump a purchase's `updated_at` without changing it (verification was inconclusive), so the
/// reconciler does not hammer the provider.
pub(crate) fn touch_purchase(
    c: &Connection,
    source: &str,
    env: &str,
    reference: &str,
    now: u64,
) -> Result<(), StoreError> {
    c.execute(
        "UPDATE nest_store_purchases SET updated_at=?4 WHERE source=?1 AND environment=?2 AND reference=?3",
        params![source, env, reference, now as i64],
    )?;
    Ok(())
}

// ---- allowance ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Funding {
    pub entitlement_id: String,
    pub source: String,
    pub package: &'static str,
    pub status: String,
    pub paid_through: Option<i64>,
    pub cancel_requested_at: Option<i64>,
    pub pending_plan_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Allowance {
    /// The live package that funds this account's Nests, if any.
    pub funding: Option<Funding>,
    /// Nest slots the funding package grants (0 without one).
    pub slots: i64,
    /// Package-funded Nests that are not deleted.
    pub used: i64,
    /// Other live Nest packages on the same account. They never add slots; each needs a refund
    /// or a cancellation (the store's own manage page).
    pub duplicates: Vec<Funding>,
}

/// Who pays for the Nests held by `account`: the account itself, or — for an organization's
/// internal account — the organization (Nests B7).
pub(crate) fn funding_subject(
    c: &Connection,
    account: &str,
) -> Result<(&'static str, String), StoreError> {
    let org: Option<String> = c
        .query_row(
            "SELECT id FROM orgs WHERE internal_account_id = ?1",
            [account],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match org {
        Some(org) => ("org", org),
        None => ("account", account.to_string()),
    })
}

/// The account's Nest allowance: its best live Nest entitlement decides it.
pub(crate) fn allowance(c: &Connection, account: &str, now: u64) -> Result<Allowance, StoreError> {
    let (kind, subject) = funding_subject(c, account)?;
    let mut live: Vec<(Funding, &'static Package)> =
        entitlements::for_subject(c, kind, &subject, now)?
            .into_iter()
            .filter(|e| e.active)
            .filter_map(|e| {
                let package = nest_catalog::for_plan_code(&e.plan_code)?;
                Some((
                    Funding {
                        entitlement_id: e.id,
                        source: e.source,
                        package: package.code,
                        status: e.status,
                        paid_through: e.paid_through,
                        cancel_requested_at: e.cancel_requested_at,
                        pending_plan_code: e.pending_plan_code,
                    },
                    package,
                ))
            })
            .collect();
    // Highest rank first; then the one paid furthest ahead; then a stable id order.
    live.sort_by(|(fa, pa), (fb, pb)| {
        pb.rank
            .cmp(&pa.rank)
            .then(
                fb.paid_through
                    .unwrap_or(i64::MAX)
                    .cmp(&fa.paid_through.unwrap_or(i64::MAX)),
            )
            .then(fa.entitlement_id.cmp(&fb.entitlement_id))
    });
    let used: i64 = c.query_row(
        "SELECT COUNT(*) FROM nests WHERE account_id=?1 AND funding='package' AND deleted_at IS NULL AND state != 'failed'",
        [account],
        |r| r.get(0),
    )?;
    let mut iter = live.into_iter();
    let first = iter.next();
    Ok(Allowance {
        slots: first.as_ref().map_or(0, |(_, p)| p.slots),
        funding: first.map(|(f, _)| f),
        used,
        duplicates: iter.map(|(f, _)| f).collect(),
    })
}

// ---- purchase intents ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentOutcome {
    Created {
        id: String,
        expires_at: i64,
    },
    /// A live Nest package on another rail already funds the account: buying here would charge
    /// twice. Manage or upgrade it where it was bought.
    ActiveElsewhere {
        source: String,
        package: &'static str,
    },
    /// The same package is already live on this rail.
    AlreadyOwned {
        package: &'static str,
    },
    /// Too many intents in the last hour.
    RateLimited,
}

/// Create a server-side purchase intent before a store sheet or web checkout is shown. Refuses a
/// purchase that would fund the account twice: only an upgrade or a change on the rail that
/// already funds it, or a first package, is allowed.
pub(crate) fn create_intent(
    c: &Connection,
    account: &str,
    package: &'static Package,
    rail: &str,
    now: u64,
) -> Result<IntentOutcome, StoreError> {
    let recent: i64 = c.query_row(
        "SELECT COUNT(*) FROM nest_purchase_intents WHERE account_id=?1 AND created_at > ?2",
        params![account, now as i64 - 3600],
        |r| r.get(0),
    )?;
    if recent >= 20 {
        return Ok(IntentOutcome::RateLimited);
    }
    let allowance = allowance(c, account, now)?;
    if let Some(f) = &allowance.funding {
        let funding_rail = match f.source.as_str() {
            "apple" => "apple",
            "google" => "google",
            "iyzico" => "web",
            // A complimentary or test grant does not block buying a real package.
            _ => "",
        };
        if !funding_rail.is_empty() {
            if funding_rail != rail {
                return Ok(IntentOutcome::ActiveElsewhere {
                    source: f.source.clone(),
                    package: f.package,
                });
            }
            if f.package == package.code {
                return Ok(IntentOutcome::AlreadyOwned { package: f.package });
            }
        }
    }
    let id = format!("npi_{}", super::random_hex(16));
    let expires = now as i64 + INTENT_SECS as i64;
    c.execute(
        "INSERT INTO nest_purchase_intents (id,account_id,package,rail,catalog_version,created_at,expires_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            id,
            account,
            package.code,
            rail,
            nest_catalog::CATALOG_VERSION,
            now as i64,
            expires
        ],
    )?;
    Ok(IntentOutcome::Created {
        id,
        expires_at: expires,
    })
}

/// Mark the newest open intent of `account` for `package` on `rail` as consumed by `reference`.
/// Purely a record: the provider verification, not the intent, grants access.
pub(crate) fn consume_intent(
    c: &Connection,
    account: &str,
    package: &str,
    rail: &str,
    reference: &str,
    now: u64,
) -> Result<(), StoreError> {
    c.execute(
        "UPDATE nest_purchase_intents SET consumed_at=?5, reference=?4
          WHERE id = (SELECT id FROM nest_purchase_intents WHERE account_id=?1 AND package=?2 AND rail=?3
                        AND consumed_at IS NULL ORDER BY created_at DESC LIMIT 1)",
        params![account, package, rail, reference, now as i64],
    )?;
    Ok(())
}

// ---- setups (paid, setup pending) -------------------------------------------------------------

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Setup {
    pub setup_id: String,
    pub entitlement_id: String,
    pub label: String,
    pub status: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub nest_id: Option<String>,
}

const SETUP_COLUMNS: &str =
    "id,entitlement_id,label,status,attempts,next_attempt_at,last_error,nest_id";

fn setup_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Setup> {
    Ok(Setup {
        setup_id: r.get(0)?,
        entitlement_id: r.get(1)?,
        label: r.get(2)?,
        status: r.get(3)?,
        attempts: r.get(4)?,
        next_attempt_at: r.get(5)?,
        last_error: r.get(6)?,
        nest_id: r.get(7)?,
    })
}

/// After a verified purchase: make sure its first Nest is set up. One setup per funding
/// entitlement, ever — a restore, a renewal or a repeated claim never queues a second Nest. Nothing
/// is queued when the account already has a package-funded Nest.
pub(crate) fn ensure_setup(
    c: &Connection,
    account: &str,
    entitlement_id: &str,
    label: &str,
    now: u64,
) -> Result<Option<Setup>, StoreError> {
    if let Some(existing) = c
        .query_row(
            &format!("SELECT {SETUP_COLUMNS} FROM nest_setups WHERE entitlement_id=?1"),
            [entitlement_id],
            setup_from,
        )
        .optional()?
    {
        return Ok(Some(existing));
    }
    let has_nest: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM nests WHERE account_id=?1 AND funding='package' AND deleted_at IS NULL)",
        [account],
        |r| r.get(0),
    )?;
    if has_nest {
        return Ok(None);
    }
    // Another pending setup on the account (an earlier package that was replaced by an upgrade)
    // already covers the first Nest.
    let pending: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM nest_setups WHERE account_id=?1 AND status IN ('pending','failed'))",
        [account],
        |r| r.get(0),
    )?;
    if pending {
        return Ok(None);
    }
    let id = format!("nsu_{}", super::random_hex(12));
    c.execute(
        "INSERT INTO nest_setups (id,account_id,entitlement_id,label,status,attempts,next_attempt_at,created_at,updated_at)
         VALUES (?1,?2,?3,?4,'pending',0,?5,?5,?5)",
        params![id, account, entitlement_id, label, now as i64],
    )?;
    Ok(c.query_row(
        &format!("SELECT {SETUP_COLUMNS} FROM nest_setups WHERE id=?1"),
        [&id],
        setup_from,
    )
    .optional()?)
}

/// A live funding package with no Nest and no setup yet (a web purchase, a grant, a restore on
/// the web): queue its first Nest. One setup per entitlement, ever, so deleting one's last Nest
/// never brings it back.
pub(crate) fn ensure_funded_setup(
    c: &Connection,
    account: &str,
    now: u64,
) -> Result<Option<Setup>, StoreError> {
    // An organization's Nests are created by its admins under handles they choose (B7), never
    // set up automatically under the org's internal account.
    if funding_subject(c, account)?.0 == "org" {
        return Ok(None);
    }
    let allowance = allowance(c, account, now)?;
    let Some(funding) = allowance.funding else {
        return Ok(None);
    };
    if allowance.used > 0 {
        return Ok(None);
    }
    let label = nest_catalog::by_code(funding.package).map_or("Nest", |p| p.name);
    ensure_setup(c, account, &funding.entitlement_id, label, now)
}

pub(crate) fn setups_for(c: &Connection, account: &str) -> Result<Vec<Setup>, StoreError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {SETUP_COLUMNS} FROM nest_setups WHERE account_id=?1 AND status IN ('pending','failed') ORDER BY created_at"
    ))?;
    let rows = stmt
        .query_map([account], setup_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Setups due for an attempt, with their account.
pub(crate) fn due_setups(c: &Connection, now: u64) -> Result<Vec<(String, Setup)>, StoreError> {
    let mut stmt = c.prepare(&format!(
        "SELECT account_id,{SETUP_COLUMNS} FROM nest_setups WHERE status='pending' AND next_attempt_at <= ?1 ORDER BY next_attempt_at LIMIT 20"
    ))?;
    let rows = stmt
        .query_map([now as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Setup {
                    setup_id: r.get(1)?,
                    entitlement_id: r.get(2)?,
                    label: r.get(3)?,
                    status: r.get(4)?,
                    attempts: r.get(5)?,
                    next_attempt_at: r.get(6)?,
                    last_error: r.get(7)?,
                    nest_id: r.get(8)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Record the outcome of one setup attempt.
pub(crate) fn finish_setup(
    c: &Connection,
    setup_id: &str,
    result: Result<&str, &str>,
    now: u64,
) -> Result<(), StoreError> {
    match result {
        Ok(nest_id) => {
            c.execute(
                "UPDATE nest_setups SET status='done', nest_id=?2, last_error=NULL, attempts=attempts+1, updated_at=?3 WHERE id=?1",
                params![setup_id, nest_id, now as i64],
            )?;
        }
        Err(error) => {
            let attempts: i64 = c.query_row(
                "SELECT attempts FROM nest_setups WHERE id=?1",
                [setup_id],
                |r| r.get(0),
            )?;
            let attempts = attempts + 1;
            let backoff = (60i64 << attempts.min(6)).min(SETUP_MAX_BACKOFF_SECS);
            let status = if attempts >= SETUP_MAX_ATTEMPTS {
                "failed"
            } else {
                "pending"
            };
            let error: String = error.chars().take(200).collect();
            c.execute(
                "UPDATE nest_setups SET status=?2, attempts=?3, next_attempt_at=?4, last_error=?5, updated_at=?6 WHERE id=?1",
                params![setup_id, status, attempts, now as i64 + backoff, error, now as i64],
            )?;
        }
    }
    Ok(())
}

/// Not attempted now (the feature is off for the account): look again in ten minutes, without
/// counting an attempt.
pub(crate) fn defer_setup(c: &Connection, setup_id: &str, now: u64) -> Result<(), StoreError> {
    c.execute(
        "UPDATE nest_setups SET next_attempt_at=?2, last_error='feature_disabled', updated_at=?3 WHERE id=?1",
        params![setup_id, now as i64 + 600, now as i64],
    )?;
    Ok(())
}

/// The owner asks for another attempt now (a `failed` setup becomes `pending` again).
pub(crate) fn retry_setup(
    c: &Connection,
    account: &str,
    setup_id: &str,
    now: u64,
) -> Result<bool, StoreError> {
    Ok(c.execute(
        "UPDATE nest_setups SET status='pending', next_attempt_at=?3, attempts=MIN(attempts, ?4), updated_at=?3
          WHERE id=?1 AND account_id=?2 AND status IN ('pending','failed')",
        params![setup_id, account, now as i64, SETUP_MAX_ATTEMPTS - 1],
    )? == 1)
}

/// A setup whose funding is gone is canceled, not attempted (no unfunded Nest is ever created).
pub(crate) fn cancel_setup(c: &Connection, setup_id: &str, now: u64) -> Result<(), StoreError> {
    c.execute(
        "UPDATE nest_setups SET status='canceled', updated_at=?2 WHERE id=?1",
        params![setup_id, now as i64],
    )?;
    Ok(())
}

// ---- funding sweep --------------------------------------------------------------------------------

/// What the funding sweep wants done to one Nest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FundingAction {
    /// Over the allowance: stop it running. `unfunded` = no live package at all (the export
    /// window starts); otherwise a downgrade left more Nests than slots (never deleted for that).
    Pause { nest_id: String, unfunded: bool },
    /// Funding is back (renewal, restore, upgrade): undo a funding pause.
    Resume { nest_id: String },
    /// Unfunded for longer than the export window, or a provisioning attempt that failed (it
    /// holds no slot; its half-made runtime and mailbox are cleaned up): delete.
    Delete { nest_id: String },
}

/// Decide the funding actions for one account. Pure decision over stored state; the caller
/// performs each one through nestd and records it with [`apply_funding`].
///
/// Rules: the oldest Nests keep the slots; Nests beyond them are paused (`over_capacity`), or,
/// without any live package, paused as `unfunded` and deleted after [`EXPORT_WINDOW_SECS`].
/// Only Nests the owner did not pause themselves are resumed when funding returns.
pub(crate) fn funding_actions(
    c: &Connection,
    account: &str,
    now: u64,
) -> Result<Vec<FundingAction>, StoreError> {
    let allowance = allowance(c, account, now)?;
    let mut stmt = c.prepare(
        "SELECT id, state, paused_reason, unfunded_since FROM nests
          WHERE account_id=?1 AND funding='package' AND deleted_at IS NULL AND state NOT IN ('creating','failed')
          ORDER BY created_at, id",
    )?;
    let nests = stmt
        .query_map([account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let unfunded = allowance.funding.is_none();
    let mut actions = Vec::new();
    // Failed provisioning attempts first: they never count against the slots.
    let mut failed = c.prepare(
        "SELECT id FROM nests WHERE account_id=?1 AND funding='package' AND deleted_at IS NULL AND state='failed' ORDER BY created_at, id",
    )?;
    for id in failed.query_map([account], |r| r.get::<_, String>(0))? {
        actions.push(FundingAction::Delete { nest_id: id? });
    }
    for (index, (id, state, reason, since)) in nests.into_iter().enumerate() {
        let within = (index as i64) < allowance.slots;
        let funding_paused = matches!(reason.as_deref(), Some("unfunded" | "over_capacity"));
        if within {
            if funding_paused {
                actions.push(FundingAction::Resume { nest_id: id });
            }
            continue;
        }
        if unfunded {
            if since.is_some_and(|s| now as i64 >= s + EXPORT_WINDOW_SECS) {
                actions.push(FundingAction::Delete { nest_id: id });
            } else if reason.as_deref() != Some("unfunded") || state != "paused" {
                actions.push(FundingAction::Pause {
                    nest_id: id,
                    unfunded: true,
                });
            }
        } else if reason.as_deref() != Some("over_capacity") || state != "paused" {
            actions.push(FundingAction::Pause {
                nest_id: id,
                unfunded: false,
            });
        }
    }
    Ok(actions)
}

/// Record a funding action that nestd carried out.
pub(crate) fn apply_funding(
    c: &Connection,
    action: &FundingAction,
    paid_through: Option<i64>,
    now: u64,
) -> Result<(), StoreError> {
    let now = now as i64;
    match action {
        FundingAction::Pause { nest_id, unfunded } => {
            // The export window runs from the end of the last paid period, never from a later
            // sweep (a delayed sweep cannot extend it, an early one cannot shorten it).
            let since = paid_through.unwrap_or(now).min(now);
            c.execute(
                "UPDATE nests SET state='paused', paused_reason=?2,
                        unfunded_since=CASE WHEN ?2='unfunded' THEN COALESCE(unfunded_since, ?3) ELSE NULL END,
                        updated_at=?4 WHERE id=?1",
                params![
                    nest_id,
                    if *unfunded { "unfunded" } else { "over_capacity" },
                    since,
                    now
                ],
            )?;
        }
        FundingAction::Resume { nest_id } => {
            c.execute(
                "UPDATE nests SET state='running', paused_reason=NULL, unfunded_since=NULL, updated_at=?2 WHERE id=?1",
                params![nest_id, now],
            )?;
        }
        FundingAction::Delete { .. } => {}
    }
    Ok(())
}

/// The most recent paid-through of any Nest entitlement the account ever had (for the export
/// window's start).
pub(crate) fn last_paid_through(c: &Connection, account: &str) -> Result<Option<i64>, StoreError> {
    let mut best: Option<i64> = None;
    let (kind, subject) = funding_subject(c, account)?;
    for e in entitlements::for_subject(c, kind, &subject, 0)? {
        if nest_catalog::for_plan_code(&e.plan_code).is_some() {
            best = best.max(e.paid_through);
        }
    }
    Ok(best)
}

/// Accounts with package-funded Nests or Nest entitlements (the sweep's work list).
pub(crate) fn funded_accounts(c: &Connection) -> Result<Vec<String>, StoreError> {
    let mut stmt = c.prepare(
        "SELECT DISTINCT account_id FROM nests WHERE funding='package' AND deleted_at IS NULL
         UNION SELECT DISTINCT subject_id FROM entitlements WHERE subject_kind='account' AND (plan_code LIKE 'nest.%' OR plan_code LIKE 'nest-%')
         UNION SELECT DISTINCT o.internal_account_id FROM entitlements e JOIN orgs o ON o.id = e.subject_id
               WHERE e.subject_kind='org' AND o.internal_account_id IS NOT NULL AND (e.plan_code LIKE 'nest.%' OR e.plan_code LIKE 'nest-%')",
    )?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    const DAY: i64 = 86400;
    const NOW: u64 = 1_900_000_000;

    fn conn(s: &Store) -> std::sync::MutexGuard<'_, Connection> {
        s.conn.lock().unwrap()
    }
    fn store() -> Store {
        let s = Store::open(":memory:").unwrap();
        for a in ["acct_a", "acct_b"] {
            conn(&s)
                .execute(
                    "INSERT INTO accounts(id,created_at,oidc_sub) VALUES(?1,1,?1)",
                    [a],
                )
                .unwrap();
        }
        s
    }
    fn apple(reference: &str, package: &str, expires: i64, state: &str) -> StorePurchase {
        StorePurchase {
            source: "apple",
            environment: "live".into(),
            reference: reference.into(),
            product_id: nest_catalog::by_code(package).unwrap().apple_product.into(),
            expires_at: expires,
            state: state.into(),
            acknowledged: true,
            replaces: None,
        }
    }
    fn google(reference: &str, package: &str, replaces: Option<&str>) -> StorePurchase {
        StorePurchase {
            source: "google",
            environment: "live".into(),
            reference: reference.into(),
            product_id: nest_catalog::by_code(package)
                .unwrap()
                .google_product
                .into(),
            expires_at: NOW as i64 + 30 * DAY,
            state: "active".into(),
            acknowledged: false,
            replaces: replaces.map(str::to_string),
        }
    }
    fn nest(c: &Connection, id: &str, account: &str, created: i64) {
        c.execute(
            "INSERT INTO nests (id,account_id,address,label,state,created_at,updated_at,funding)
             VALUES (?1,?2,?3,'n','running',?4,?4,'package')",
            params![id, account, format!("/k/{id}"), created],
        )
        .unwrap();
    }
    fn web_grant(c: &Connection, account: &str, code: &str, reference: &str, paid: Option<i64>) {
        let input = EntitlementInput {
            source: "iyzico".into(),
            environment: "live".into(),
            reference: reference.into(),
            subject_kind: "account".into(),
            subject_id: account.into(),
            plan_code: code.into(),
            plan_version: "2026".into(),
            names: None,
            seats: None,
            agent_limit: None,
            storage_bytes: None,
            status: "active".into(),
            period_end: paid,
            paid_through: paid,
            cancel_requested_at: None,
            pending_plan_code: None,
            pending_effective_at: None,
            reconciliation_version: 1,
            expected_oidc_sub: None,
        };
        assert!(matches!(
            entitlements::upsert(c, &input, NOW).unwrap(),
            entitlements::UpsertOutcome::Applied(_)
        ));
    }

    #[test]
    fn a_verified_store_purchase_funds_slots_but_never_web_names() {
        let s = store();
        let c = conn(&s);
        let claim = record_store_purchase(
            &c,
            "acct_a",
            &apple(
                "1000",
                "nest.ai.starter.monthly",
                NOW as i64 + 30 * DAY,
                "active",
            ),
            NOW,
        )
        .unwrap();
        let StoreClaim::Saved {
            active,
            duplicate,
            package,
            ..
        } = claim
        else {
            panic!("{claim:?}")
        };
        assert!(active && !duplicate);
        assert_eq!(package, "nest.ai.starter.monthly");
        let a = allowance(&c, "acct_a", NOW).unwrap();
        assert_eq!((a.slots, a.used), (1, 0));
        assert_eq!(a.funding.unwrap().source, "apple");
        // A Nest package is not a web name plan.
        assert_eq!(
            entitlements::web_plan_for_test(&c, "acct_a", NOW).capacity,
            0
        );
        assert!(!entitlements::web_plan_for_test(&c, "acct_a", NOW).active);
        // Another account gets nothing, and cannot take the subscription over.
        assert_eq!(allowance(&c, "acct_b", NOW).unwrap().slots, 0);
        assert_eq!(
            record_store_purchase(
                &c,
                "acct_b",
                &apple(
                    "1000",
                    "nest.ai.starter.monthly",
                    NOW as i64 + 30 * DAY,
                    "active"
                ),
                NOW
            )
            .unwrap(),
            StoreClaim::BoundElsewhere
        );
        // Unknown products grant nothing.
        let mut other = apple(
            "2000",
            "nest.ai.starter.monthly",
            NOW as i64 + DAY,
            "active",
        );
        other.product_id = "dev.pigeonpost.inbox.handles.1.yearly".into();
        assert_eq!(
            record_store_purchase(&c, "acct_a", &other, NOW).unwrap(),
            StoreClaim::UnknownProduct
        );
    }

    #[test]
    fn a_second_live_package_is_a_duplicate_and_never_adds_slots() {
        let s = store();
        let c = conn(&s);
        record_store_purchase(
            &c,
            "acct_a",
            &apple(
                "1000",
                "nest.ai.plus.monthly",
                NOW as i64 + 30 * DAY,
                "active",
            ),
            NOW,
        )
        .unwrap();
        let claim = record_store_purchase(
            &c,
            "acct_a",
            &google("tok1", "nest.ai.starter.monthly", None),
            NOW,
        )
        .unwrap();
        assert!(matches!(
            claim,
            StoreClaim::Saved {
                duplicate: true,
                active: true,
                ..
            }
        ));
        let a = allowance(&c, "acct_a", NOW).unwrap();
        assert_eq!(
            a.slots, 3,
            "the highest package funds, the other is not added"
        );
        assert_eq!(a.funding.unwrap().package, "nest.ai.plus.monthly");
        assert_eq!(a.duplicates.len(), 1);
        assert_eq!(a.duplicates[0].source, "google");
    }

    #[test]
    fn intents_refuse_a_second_charge_and_allow_upgrades_on_the_same_rail() {
        let s = store();
        let c = conn(&s);
        let starter = nest_catalog::by_code("nest.ai.starter.monthly").unwrap();
        let plus = nest_catalog::by_code("nest.ai.plus.monthly").unwrap();
        // First purchase: any rail.
        assert!(matches!(
            create_intent(&c, "acct_a", starter, "web", NOW).unwrap(),
            IntentOutcome::Created { .. }
        ));
        record_store_purchase(
            &c,
            "acct_a",
            &apple(
                "1000",
                "nest.ai.starter.monthly",
                NOW as i64 + 30 * DAY,
                "active",
            ),
            NOW,
        )
        .unwrap();
        assert!(matches!(
            create_intent(&c, "acct_a", plus, "web", NOW).unwrap(),
            IntentOutcome::ActiveElsewhere { .. }
        ));
        assert!(matches!(
            create_intent(&c, "acct_a", plus, "google", NOW).unwrap(),
            IntentOutcome::ActiveElsewhere { .. }
        ));
        assert!(matches!(
            create_intent(&c, "acct_a", starter, "apple", NOW).unwrap(),
            IntentOutcome::AlreadyOwned { .. }
        ));
        assert!(matches!(
            create_intent(&c, "acct_a", plus, "apple", NOW).unwrap(),
            IntentOutcome::Created { .. }
        ));
        // A complimentary grant does not block a real purchase.
        let grant = EntitlementInput {
            source: "grant".into(),
            environment: "live".into(),
            reference: "nest-grant-b".into(),
            subject_kind: "account".into(),
            subject_id: "acct_b".into(),
            plan_code: "nest.ai.starter.monthly".into(),
            plan_version: "grant".into(),
            names: None,
            seats: None,
            agent_limit: None,
            storage_bytes: None,
            status: "active".into(),
            period_end: None,
            paid_through: None,
            cancel_requested_at: None,
            pending_plan_code: None,
            pending_effective_at: None,
            reconciliation_version: 1,
            expected_oidc_sub: None,
        };
        entitlements::upsert(&c, &grant, NOW).unwrap();
        assert_eq!(allowance(&c, "acct_b", NOW).unwrap().slots, 1);
        assert!(matches!(
            create_intent(&c, "acct_b", starter, "google", NOW).unwrap(),
            IntentOutcome::Created { .. }
        ));
        // Bounded: at most 20 intents an hour.
        for _ in 0..19 {
            create_intent(&c, "acct_b", starter, "google", NOW).unwrap();
        }
        assert_eq!(
            create_intent(&c, "acct_b", starter, "google", NOW).unwrap(),
            IntentOutcome::RateLimited
        );
    }

    #[test]
    fn a_replaced_or_refunded_purchase_stops_at_once_and_stays_stopped() {
        let s = store();
        let c = conn(&s);
        record_store_purchase(
            &c,
            "acct_a",
            &google("old", "nest.ai.starter.monthly", None),
            NOW,
        )
        .unwrap();
        // Upgrade: the new token names the old one.
        record_store_purchase(
            &c,
            "acct_a",
            &google("new", "nest.ai.plus.monthly", Some("old")),
            NOW,
        )
        .unwrap();
        let a = allowance(&c, "acct_a", NOW + 1).unwrap();
        assert_eq!(a.slots, 3);
        assert!(
            a.duplicates.is_empty(),
            "the replaced token is not a duplicate"
        );
        // A late verification still calling the old token active cannot revive it.
        record_store_purchase(
            &c,
            "acct_a",
            &google("old", "nest.ai.starter.monthly", None),
            NOW + 2,
        )
        .unwrap();
        assert!(allowance(&c, "acct_a", NOW + 3)
            .unwrap()
            .duplicates
            .is_empty());
        // Refund of the new one: no funding at all, immediately (not at the period end).
        let mut refunded = google("new", "nest.ai.plus.monthly", None);
        refunded.state = "revoked".into();
        record_store_purchase(&c, "acct_a", &refunded, NOW + 10).unwrap();
        let a = allowance(&c, "acct_a", NOW + 11).unwrap();
        assert!(a.funding.is_none(), "{a:?}");
        // Replacing someone else's token is refused.
        record_store_purchase(
            &c,
            "acct_b",
            &google("b1", "nest.ai.starter.monthly", None),
            NOW,
        )
        .unwrap();
        assert_eq!(
            record_store_purchase(
                &c,
                "acct_a",
                &google("a2", "nest.ai.plus.monthly", Some("b1")),
                NOW
            )
            .unwrap(),
            StoreClaim::BoundElsewhere
        );
    }

    #[test]
    fn funding_pauses_excess_resumes_on_renewal_and_deletes_only_after_the_window() {
        let s = store();
        let c = conn(&s);
        let paid = NOW as i64 + 10 * DAY;
        record_store_purchase(
            &c,
            "acct_a",
            &apple("1000", "nest.ai.plus.monthly", paid, "active"),
            NOW,
        )
        .unwrap();
        for (i, id) in ["nst_01", "nst_02", "nst_03"].iter().enumerate() {
            nest(&c, id, "acct_a", 100 + i as i64);
        }
        assert!(funding_actions(&c, "acct_a", NOW).unwrap().is_empty());
        // Downgrade to one slot: the two newest pause, nothing is deleted, ever.
        record_store_purchase(
            &c,
            "acct_a",
            &apple("1000", "nest.ai.starter.monthly", paid, "active"),
            NOW + 1,
        )
        .unwrap();
        let actions = funding_actions(&c, "acct_a", NOW + 1).unwrap();
        assert_eq!(
            actions,
            vec![
                FundingAction::Pause {
                    nest_id: "nst_02".into(),
                    unfunded: false
                },
                FundingAction::Pause {
                    nest_id: "nst_03".into(),
                    unfunded: false
                },
            ]
        );
        for a in &actions {
            apply_funding(&c, a, Some(paid), NOW + 1).unwrap();
        }
        assert!(funding_actions(&c, "acct_a", NOW + 400 * DAY as u64 / 400)
            .unwrap()
            .is_empty());
        // Expiry: everything pauses as unfunded; the export window runs from the paid-through.
        let after = (paid + 1) as u64;
        let actions = funding_actions(&c, "acct_a", after).unwrap();
        assert_eq!(actions.len(), 3, "{actions:?}");
        assert!(actions
            .iter()
            .all(|a| matches!(a, FundingAction::Pause { unfunded: true, .. })));
        for a in &actions {
            apply_funding(&c, a, last_paid_through(&c, "acct_a").unwrap(), after).unwrap();
        }
        assert!(funding_actions(&c, "acct_a", after + 3600)
            .unwrap()
            .is_empty());
        let window_end = (paid + EXPORT_WINDOW_SECS) as u64;
        assert!(funding_actions(&c, "acct_a", window_end - 1)
            .unwrap()
            .is_empty());
        // Renewal before the window ends: the slots' Nests resume, the excess stays paused.
        record_store_purchase(
            &c,
            "acct_a",
            &apple("1000", "nest.ai.starter.monthly", paid + 40 * DAY, "active"),
            after + 7200,
        )
        .unwrap();
        let actions = funding_actions(&c, "acct_a", after + 7200).unwrap();
        assert_eq!(
            actions[0],
            FundingAction::Resume {
                nest_id: "nst_01".into()
            }
        );
        assert!(actions[1..].iter().all(|a| matches!(
            a,
            FundingAction::Pause {
                unfunded: false,
                ..
            }
        )));
        for a in &actions {
            apply_funding(&c, a, Some(paid + 40 * DAY), after + 7200).unwrap();
        }
        // Lapse again and wait out the window: only now is anything deleted.
        let lapsed = (paid + 40 * DAY + 1) as u64;
        for a in funding_actions(&c, "acct_a", lapsed).unwrap() {
            apply_funding(&c, &a, last_paid_through(&c, "acct_a").unwrap(), lapsed).unwrap();
        }
        let end = (paid + 40 * DAY + EXPORT_WINDOW_SECS) as u64;
        let actions = funding_actions(&c, "acct_a", end).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(actions
            .iter()
            .all(|a| matches!(a, FundingAction::Delete { .. })));
        // Ops-funded (Phase A) Nests are never touched.
        c.execute("UPDATE nests SET funding='ops'", []).unwrap();
        assert!(funding_actions(&c, "acct_a", end).unwrap().is_empty());
    }

    #[test]
    fn one_setup_per_entitlement_with_backoff_and_owner_retry() {
        let s = store();
        let c = conn(&s);
        web_grant(
            &c,
            "acct_a",
            "nest-ai-starter-monthly-usd",
            "sub-1",
            Some(NOW as i64 + 30 * DAY),
        );
        let first = ensure_funded_setup(&c, "acct_a", NOW).unwrap().unwrap();
        assert_eq!(first.status, "pending");
        assert_eq!(first.label, "Pigeonpost AI");
        // Again (restore, renewal, a second sweep): the same setup, never a second one.
        assert_eq!(
            ensure_funded_setup(&c, "acct_a", NOW + 5)
                .unwrap()
                .unwrap()
                .setup_id,
            first.setup_id
        );
        assert_eq!(due_setups(&c, NOW).unwrap().len(), 1);
        // Failures back off and end in `failed` after the attempt limit.
        for i in 0..SETUP_MAX_ATTEMPTS {
            finish_setup(&c, &first.setup_id, Err("nestd_failed"), NOW + i as u64).unwrap();
        }
        let failed = &setups_for(&c, "acct_a").unwrap()[0];
        assert_eq!(failed.status, "failed");
        assert!(due_setups(&c, NOW + 10 * DAY as u64).unwrap().is_empty());
        // The owner retries: due at once, one attempt left before failing again.
        assert!(retry_setup(&c, "acct_a", &first.setup_id, NOW + 100).unwrap());
        assert!(
            !retry_setup(&c, "acct_b", &first.setup_id, NOW + 100).unwrap(),
            "only the owner"
        );
        assert_eq!(due_setups(&c, NOW + 100).unwrap().len(), 1);
        finish_setup(&c, &first.setup_id, Ok("nst_99"), NOW + 101).unwrap();
        assert!(setups_for(&c, "acct_a").unwrap().is_empty());
        // Deleting one's Nest later never brings a setup back for the same entitlement.
        assert_eq!(
            ensure_funded_setup(&c, "acct_a", NOW + 200)
                .unwrap()
                .unwrap()
                .status,
            "done"
        );
        // An account that already has a package Nest gets no setup.
        web_grant(
            &c,
            "acct_b",
            "nest-ai-plus-monthly-eur",
            "sub-2",
            Some(NOW as i64 + DAY),
        );
        nest(&c, "nst_b1", "acct_b", 1);
        assert!(ensure_funded_setup(&c, "acct_b", NOW).unwrap().is_none());
        // No funding, no setup.
        c.execute("DELETE FROM entitlements WHERE subject_id='acct_b'", [])
            .unwrap();
        c.execute("DELETE FROM nests WHERE account_id='acct_b'", [])
            .unwrap();
        assert!(ensure_funded_setup(&c, "acct_b", NOW).unwrap().is_none());
    }
}
