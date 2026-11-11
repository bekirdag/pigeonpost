//! Verified publisher registration (developer program plan §2–§3): applications with an account
//! type, documents, KYC checks, the review history, notices to the developer and the payee push to
//! saas_be.
//!
//! Additive only. `publisher_profiles` keeps its shape and its status values: an application is a
//! separate record that certifies a publisher profile when staff approve it. Every existing
//! publisher profile gets a `legacy` application row (the M0 one-field application), so lists and
//! the staff queue see one model; nothing about an existing publisher changes.
//!
//! Sensitive fields are never stored in the clear: bank and tax details are sealed under the vault
//! (`bank_sealed`, `tax_sealed`, label `publisher-payee-v1`, bound to the application id) next to a
//! masked copy for reads. Document bytes are sealed the same way and kept in the blob store
//! (`<blob dir>/publisherdocs`), never in this database.

use rusqlite::{params, Connection, OptionalExtension};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS publisher_applications (
    id            TEXT PRIMARY KEY,
    owner_kind    TEXT NOT NULL CHECK (owner_kind IN ('account','org')),
    owner_id      TEXT NOT NULL,
    applicant     TEXT NOT NULL,
    account_type  TEXT NOT NULL CHECK (account_type IN
                      ('individual_tr','individual_foreign','sole_proprietor_tr','company_tr',
                       'company_foreign','legacy')),
    state         TEXT NOT NULL CHECK (state IN
                      ('draft','submitted','in_review','needs_info','approved','rejected')),
    -- The publisher profile this application certifies: set for legacy rows and re-verifications,
    -- and on approval otherwise.
    publisher_id  TEXT REFERENCES publisher_profiles(id),
    public_name   TEXT NOT NULL DEFAULT '',
    -- Non-secret application fields (legal and trading name, address, website, contacts,
    -- registration numbers), JSON.
    profile       TEXT NOT NULL DEFAULT '{}',
    bank_sealed   BLOB,
    bank_masked   TEXT,
    tax_sealed    BLOB,
    tax_masked    TEXT,
    -- Derived from the account type (plan §3) and kept in the clear for the queue and saas_be.
    tax_category  TEXT CHECK (tax_category IS NULL OR tax_category IN
                      ('tr_business','tr_individual','foreign_company','foreign_individual')),
    agreement_version      TEXT,
    content_policy_version TEXT,
    agreement_accepted_at  INTEGER,
    -- Bumped on every change; a staff decision names the revision it read.
    revision      INTEGER NOT NULL DEFAULT 1,
    reviewer      TEXT,
    submitted_at  INTEGER,
    decided_at    INTEGER,
    decided_by    TEXT,
    decision_reason TEXT,
    checklist     TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS publisher_applications_by_owner
    ON publisher_applications(owner_kind, owner_id);
CREATE INDEX IF NOT EXISTS publisher_applications_by_state
    ON publisher_applications(state, submitted_at);
-- One open application per owner.
CREATE UNIQUE INDEX IF NOT EXISTS publisher_applications_open
    ON publisher_applications(owner_kind, owner_id)
    WHERE state IN ('draft','submitted','in_review','needs_info');

CREATE TABLE IF NOT EXISTS publisher_application_documents (
    id             TEXT PRIMARY KEY,
    application_id TEXT NOT NULL REFERENCES publisher_applications(id),
    doc_type       TEXT NOT NULL,
    filename       TEXT NOT NULL,
    content_type   TEXT NOT NULL CHECK (content_type IN ('application/pdf','image/jpeg','image/png')),
    size           INTEGER NOT NULL CHECK (size > 0 AND size <= 10485760),
    -- SHA-256 of the plaintext (what the reviewer can compare) and the blob holding the sealed
    -- bytes (content-addressed by the ciphertext).
    sha256         TEXT NOT NULL,
    blob           TEXT,
    issued_on      TEXT,
    expires_on     TEXT,
    scan_state     TEXT NOT NULL DEFAULT 'pending'
                   CHECK (scan_state IN ('pending','clean','infected','error')),
    scan_detail    TEXT,
    scanned_at     INTEGER,
    uploaded_by    TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    -- Withdrawn by the applicant before a decision, or replaced; the bytes go at `purged_at`.
    removed_at     INTEGER,
    retain_until   INTEGER,
    purged_at      INTEGER
);
CREATE INDEX IF NOT EXISTS publisher_application_documents_by_app
    ON publisher_application_documents(application_id);

-- One row per KYC verification at the provider. Only what the plan allows is kept: the reference,
-- the latest event and result, declined reasons and the verified name, date of birth and country.
-- Never an image.
CREATE TABLE IF NOT EXISTS publisher_kyc_checks (
    id             TEXT PRIMARY KEY,
    application_id TEXT NOT NULL REFERENCES publisher_applications(id),
    subject        TEXT NOT NULL CHECK (subject IN ('applicant','signatory')),
    provider       TEXT NOT NULL CHECK (provider IN ('shufti','mock')),
    reference      TEXT NOT NULL UNIQUE,
    event          TEXT NOT NULL,
    result         TEXT NOT NULL CHECK (result IN ('pending','accepted','declined','cancelled','error')),
    declined_reason TEXT,
    declined_codes  TEXT,
    aml_result     TEXT,
    verified_name  TEXT,
    verified_dob   TEXT,
    verified_country TEXT,
    verification_url TEXT,
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    final_at       INTEGER,
    polled_at      INTEGER
);
CREATE INDEX IF NOT EXISTS publisher_kyc_checks_by_app ON publisher_kyc_checks(application_id);

-- Every provider message that changed or confirmed a check, keyed by the digest of its signed body:
-- a replayed callback is recognised and changes nothing.
CREATE TABLE IF NOT EXISTS publisher_kyc_events (
    body_digest  TEXT PRIMARY KEY,
    check_id     TEXT NOT NULL REFERENCES publisher_kyc_checks(id),
    event        TEXT NOT NULL,
    source       TEXT NOT NULL CHECK (source IN ('callback','status','mock')),
    received_at  INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS publisher_kyc_events_no_update BEFORE UPDATE ON publisher_kyc_events
BEGIN SELECT RAISE(ABORT, 'publisher_kyc_events are append-only'); END;

-- The application's history as the developer and staff see it: state changes, staff requests for
-- information and the developer's replies. Append-only.
CREATE TABLE IF NOT EXISTS publisher_application_events (
    id             TEXT PRIMARY KEY,
    application_id TEXT NOT NULL REFERENCES publisher_applications(id),
    actor          TEXT NOT NULL,
    actor_role     TEXT NOT NULL CHECK (actor_role IN ('developer','staff','system')),
    kind           TEXT NOT NULL,
    from_state     TEXT,
    to_state       TEXT,
    message        TEXT,
    created_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS publisher_application_events_by_app
    ON publisher_application_events(application_id, created_at);
CREATE TRIGGER IF NOT EXISTS publisher_application_events_no_update
BEFORE UPDATE ON publisher_application_events
BEGIN SELECT RAISE(ABORT, 'publisher_application_events are append-only'); END;
CREATE TRIGGER IF NOT EXISTS publisher_application_events_no_delete
BEFORE DELETE ON publisher_application_events
BEGIN SELECT RAISE(ABORT, 'publisher_application_events are append-only'); END;

-- Durable outbox: one email and one Pigeonpost message per state change.
CREATE TABLE IF NOT EXISTS publisher_application_notices (
    id             TEXT PRIMARY KEY,
    application_id TEXT NOT NULL REFERENCES publisher_applications(id),
    channel        TEXT NOT NULL CHECK (channel IN ('email','message')),
    state          TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','sent','failed','skipped')),
    attempts       INTEGER NOT NULL DEFAULT 0,
    next_at        INTEGER NOT NULL,
    last_error     TEXT,
    created_at     INTEGER NOT NULL,
    sent_at        INTEGER
);
CREATE INDEX IF NOT EXISTS publisher_application_notices_due
    ON publisher_application_notices(status, next_at);

-- Durable outbox: the verified payee and tax profile, pushed to saas_be marketplace finance after an
-- approval. The payload is built (and opened) at send time from the sealed fields.
CREATE TABLE IF NOT EXISTS publisher_payee_pushes (
    id             TEXT PRIMARY KEY,
    application_id TEXT NOT NULL REFERENCES publisher_applications(id),
    publisher_id   TEXT NOT NULL REFERENCES publisher_profiles(id),
    approved_by    TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','sent','failed')),
    attempts       INTEGER NOT NULL DEFAULT 0,
    next_at        INTEGER NOT NULL,
    last_error     TEXT,
    remote_revision INTEGER,
    created_at     INTEGER NOT NULL,
    sent_at        INTEGER
);
CREATE INDEX IF NOT EXISTS publisher_payee_pushes_due ON publisher_payee_pushes(status, next_at);
";

/// Columns added to tables that already exist.
const COLUMNS: &[(&str, &str, &str)] = &[
    // The application whose verification certified this publisher (none for legacy approvals).
    ("publisher_profiles", "verified_application_id", "TEXT"),
];

fn has_column(c: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|n| n == column))
}

/// The legacy state an M0 publisher profile maps to. A revoked publisher was approved once; the
/// revocation is the profile's, not the application's.
pub(crate) fn legacy_state(profile_status: &str) -> &'static str {
    match profile_status {
        "applied" => "submitted",
        "rejected" => "rejected",
        _ => "approved",
    }
}

/// Run after the marketplace schemas. Idempotent.
pub(crate) fn migrate(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(SCHEMA)?;
    for (table, column, def) in COLUMNS {
        if !has_column(c, table, column)? {
            c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def}"))?;
        }
    }
    backfill_legacy(c)
}

/// One `legacy` application for every account or org publisher profile that has none.
pub(crate) fn backfill_legacy(c: &Connection) -> rusqlite::Result<()> {
    let mut stmt = c.prepare(
        "SELECT p.id, p.owner_kind, p.owner_id, p.applied_by, p.display_name, p.status,
                p.decided_by, p.decided_at, p.decision_reason, p.created_at, p.updated_at
           FROM publisher_profiles p
          WHERE p.owner_kind IN ('account','org')
            AND NOT EXISTS (SELECT 1 FROM publisher_applications a WHERE a.publisher_id = p.id)",
    )?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        i64,
        i64,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (id, kind, owner, by, name, status, decided_by, decided_at, reason, created, updated) in
        rows
    {
        let state = legacy_state(&status);
        // An owner can hold one open application; a second open legacy row (two applied profiles
        // for one owner cannot exist, but be safe) is recorded as decided.
        let open_exists: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM publisher_applications WHERE owner_kind = ?1
                AND owner_id = ?2 AND state IN ('draft','submitted','in_review','needs_info'))",
            params![kind, owner],
            |r| r.get(0),
        )?;
        let state = if state == "submitted" && open_exists {
            "rejected"
        } else {
            state
        };
        c.execute(
            "INSERT INTO publisher_applications
                (id, owner_kind, owner_id, applicant, account_type, state, publisher_id, public_name,
                 submitted_at, decided_at, decided_by, decision_reason, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'legacy', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?8, ?12)",
            params![
                format!("pap_l{}", id.trim_start_matches("pub_")),
                kind,
                owner,
                by,
                state,
                id,
                name,
                created,
                decided_at,
                decided_by,
                reason,
                updated
            ],
        )?;
    }
    Ok(())
}

/// Whether `staff` has a stake in this application: they filed it, own it, or belong to the org
/// that owns it (the same rule as [`super::marketplace::publisher_conflict_in`]). An application
/// that cannot be found counts as a conflict.
pub(crate) fn application_conflict_in(
    c: &Connection,
    staff: &str,
    application: &str,
) -> rusqlite::Result<bool> {
    let row: Option<(String, String, String, Option<String>)> = c
        .query_row(
            "SELECT owner_kind, owner_id, applicant, publisher_id FROM publisher_applications
              WHERE id = ?1",
            [application],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((kind, owner, applicant, publisher)) = row else {
        return Ok(true);
    };
    if applicant == staff || (kind == "account" && owner == staff) {
        return Ok(true);
    }
    if let Some(p) = publisher {
        if super::marketplace::publisher_conflict_in(c, staff, &p)? {
            return Ok(true);
        }
    }
    if kind != "org" {
        return Ok(false);
    }
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM org_members
                         WHERE org_id = ?1 AND account_id = ?2 AND status <> 'removed')
             OR EXISTS (SELECT 1 FROM orgs WHERE id = ?1 AND owner_account = ?2)",
        params![owner, staff],
        |r| r.get(0),
    )
}

/// A typed `/v1/events` entry for one account, in the caller's transaction.
pub(crate) fn registration_event_in(
    c: &Connection,
    account: &str,
    kind: &str,
    data: &serde_json::Value,
    now: u64,
) -> Result<(), super::StoreError> {
    super::events::append_in(c, account, kind, None, data, now)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database from before this change: publisher profiles in every M0 state, no application
    /// tables. Migrating keeps every profile as it was and gives each one a legacy application;
    /// running it again changes nothing.
    #[test]
    fn existing_publishers_map_to_legacy_applications() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(super::super::marketplace::SCHEMA).unwrap();
        for (id, kind, owner, status) in [
            ("pub_a", "account", "acct_a", "applied"),
            ("pub_b", "account", "acct_b", "approved"),
            ("pub_c", "org", "org_c", "rejected"),
            ("pub_d", "account", "acct_d", "revoked"),
            ("pub_p", "platform", "acct_owner", "approved"),
        ] {
            c.execute(
                "INSERT INTO publisher_profiles (id, owner_kind, owner_id, display_name, status,
                    applied_by, created_at, updated_at) VALUES (?1, ?2, ?3, 'Name', ?4, ?3, 5, 6)",
                params![id, kind, owner, status],
            )
            .unwrap();
        }
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let rows: Vec<(String, String, String)> = c
            .prepare(
                "SELECT publisher_id, state, account_type FROM publisher_applications
                  ORDER BY publisher_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("pub_a".into(), "submitted".into(), "legacy".into()),
                ("pub_b".into(), "approved".into(), "legacy".into()),
                ("pub_c".into(), "rejected".into(), "legacy".into()),
                ("pub_d".into(), "approved".into(), "legacy".into()),
            ]
        );
        // The profiles themselves are untouched.
        let statuses: Vec<String> = c
            .prepare("SELECT status FROM publisher_profiles ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            statuses,
            ["applied", "approved", "rejected", "revoked", "approved"]
        );
        assert!(has_column(&c, "publisher_profiles", "verified_application_id").unwrap());
    }

    #[test]
    fn a_missing_application_is_a_conflict() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(super::super::marketplace::SCHEMA).unwrap();
        c.execute_batch(
            "CREATE TABLE orgs (id TEXT, owner_account TEXT);
             CREATE TABLE org_members (org_id TEXT, account_id TEXT, status TEXT);",
        )
        .unwrap();
        migrate(&c).unwrap();
        assert!(application_conflict_in(&c, "acct_x", "pap_none").unwrap());
        c.execute(
            "INSERT INTO publisher_applications (id, owner_kind, owner_id, applicant, account_type,
                state, created_at, updated_at)
             VALUES ('pap_1', 'org', 'org_1', 'acct_dev', 'company_tr', 'submitted', 1, 1)",
            [],
        )
        .unwrap();
        assert!(application_conflict_in(&c, "acct_dev", "pap_1").unwrap());
        assert!(!application_conflict_in(&c, "acct_staff", "pap_1").unwrap());
        c.execute(
            "INSERT INTO org_members VALUES ('org_1', 'acct_staff', 'active')",
            [],
        )
        .unwrap();
        assert!(application_conflict_in(&c, "acct_staff", "pap_1").unwrap());
    }
}
