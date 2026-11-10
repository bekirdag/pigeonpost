//! Marketplace foundations (build plan §4–§5, M0): staff roles, publishers and the catalog.
//!
//! Additive tables only. `nests` is not changed: an org-owned Nest is already identified through
//! `orgs.internal_account_id` (`nest_access`), so a second `owner_kind` there would be a copy of a
//! fact that can drift. New records that can belong to a person or an org carry `owner_kind` and
//! `owner_id` here.
//!
//! Money is out of scope for these tables and always will be: commission, reserve and payout
//! values live in finance configuration, never in this schema or in tracked files.

use rusqlite::{params, Connection, OptionalExtension};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS staff_roles (
    id         TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('reviewer','finance')),
    granted_by TEXT NOT NULL,
    granted_at INTEGER NOT NULL,
    revoked_by TEXT,
    revoked_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS staff_roles_live ON staff_roles(account_id, role)
    WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS publisher_profiles (
    id           TEXT PRIMARY KEY,
    owner_kind   TEXT NOT NULL CHECK (owner_kind IN ('account','org')),
    owner_id     TEXT NOT NULL,
    display_name TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('applied','approved','rejected','revoked')),
    applied_by   TEXT NOT NULL,
    decided_by   TEXT,
    decided_at   INTEGER,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS publisher_profiles_live ON publisher_profiles(owner_kind, owner_id)
    WHERE status IN ('applied','approved');

CREATE TABLE IF NOT EXISTS nest_templates (
    id           TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    slug         TEXT NOT NULL,
    name         TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE (publisher_id, slug)
);

CREATE TABLE IF NOT EXISTS nest_releases (
    id           TEXT PRIMARY KEY,
    template_id  TEXT NOT NULL REFERENCES nest_templates(id),
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    version      TEXT NOT NULL,
    image_digest TEXT NOT NULL CHECK (image_digest LIKE 'sha256:%'),
    status       TEXT NOT NULL CHECK (status IN ('draft','submitted','approved','rejected','revoked')),
    created_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE (template_id, version)
);
CREATE INDEX IF NOT EXISTS nest_releases_by_publisher ON nest_releases(publisher_id);
-- A release is what was reviewed: its bytes and identity never change, only its status.
CREATE TRIGGER IF NOT EXISTS nest_releases_immutable BEFORE UPDATE ON nest_releases
WHEN OLD.image_digest <> NEW.image_digest OR OLD.version <> NEW.version
  OR OLD.template_id <> NEW.template_id OR OLD.publisher_id <> NEW.publisher_id
BEGIN SELECT RAISE(ABORT, 'nest_releases are immutable'); END;

CREATE TABLE IF NOT EXISTS nest_listings (
    id           TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    release_id   TEXT NOT NULL REFERENCES nest_releases(id),
    title        TEXT NOT NULL,
    summary      TEXT NOT NULL,
    visibility   TEXT NOT NULL CHECK (visibility IN ('public','unlisted','private','invite_only','organization')),
    status       TEXT NOT NULL CHECK (status IN ('draft','published','suspended','withdrawn')),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_listings_by_publisher ON nest_listings(publisher_id);

CREATE TABLE IF NOT EXISTS review_cases (
    id           TEXT PRIMARY KEY,
    release_id   TEXT NOT NULL REFERENCES nest_releases(id),
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    status       TEXT NOT NULL CHECK (status IN ('open','blocked','decided','withdrawn')),
    previous_id  TEXT REFERENCES review_cases(id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS review_cases_by_release ON review_cases(release_id);

CREATE TABLE IF NOT EXISTS review_decisions (
    id         TEXT PRIMARY KEY,
    case_id    TEXT NOT NULL REFERENCES review_cases(id),
    reviewer   TEXT NOT NULL,
    decision   TEXT NOT NULL CHECK (decision IN ('approved','rejected','changes_requested')),
    reason     TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS review_decisions_by_case ON review_decisions(case_id);
CREATE TRIGGER IF NOT EXISTS review_decisions_no_update BEFORE UPDATE ON review_decisions
BEGIN SELECT RAISE(ABORT, 'review_decisions are append-only'); END;
CREATE TRIGGER IF NOT EXISTS review_decisions_no_delete BEFORE DELETE ON review_decisions
BEGIN SELECT RAISE(ABORT, 'review_decisions are append-only'); END;

CREATE TABLE IF NOT EXISTS release_attestations (
    id               TEXT PRIMARY KEY,
    release_id       TEXT NOT NULL REFERENCES nest_releases(id),
    case_id          TEXT NOT NULL REFERENCES review_cases(id),
    attester         TEXT NOT NULL,
    statement_digest TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    UNIQUE (release_id, statement_digest)
);
CREATE TRIGGER IF NOT EXISTS release_attestations_no_update BEFORE UPDATE ON release_attestations
BEGIN SELECT RAISE(ABORT, 'release_attestations are append-only'); END;
CREATE TRIGGER IF NOT EXISTS release_attestations_no_delete BEFORE DELETE ON release_attestations
BEGIN SELECT RAISE(ABORT, 'release_attestations are append-only'); END;
";

/// `account`'s live staff roles, as stored. Text that no longer parses is ignored, so a role this
/// binary does not know grants nothing.
pub(crate) fn live_staff_roles_in(
    c: &Connection,
    account: &str,
) -> rusqlite::Result<Vec<crate::policy::StaffRole>> {
    let mut stmt =
        c.prepare("SELECT role FROM staff_roles WHERE account_id = ?1 AND revoked_at IS NULL")?;
    let rows = stmt
        .query_map([account], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .iter()
        .filter_map(|r| crate::policy::StaffRole::parse(r))
        .collect())
}

/// Whether `staff` has a stake in this publisher: they own it, filed its application, or belong to
/// the organization that owns it. A publisher that cannot be found counts as a conflict, so a
/// wrong id can never pass as "no conflict".
///
/// A suspended or invited org member still counts. Only a member who has left is clear; the
/// membership history behind that is the org's own record.
pub(crate) fn publisher_conflict_in(
    c: &Connection,
    staff: &str,
    publisher: &str,
) -> rusqlite::Result<bool> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT owner_kind, owner_id, applied_by FROM publisher_profiles WHERE id = ?1",
            [publisher],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((kind, owner, applied_by)) = row else {
        return Ok(true);
    };
    if applied_by == staff || (kind == "account" && owner == staff) {
        return Ok(true);
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

/// The conflict check for reviewing a release: its publisher's stake (above), or having built it.
/// A release that cannot be found counts as a conflict.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn release_conflict_in(
    c: &Connection,
    staff: &str,
    release: &str,
) -> rusqlite::Result<bool> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT publisher_id, created_by FROM nest_releases WHERE id = ?1",
            [release],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((publisher, author)) = row else {
        return Ok(true);
    };
    Ok(author == staff || publisher_conflict_in(c, staff, &publisher)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("
            CREATE TABLE orgs (id TEXT PRIMARY KEY, owner_account TEXT);
            CREATE TABLE org_members (id TEXT PRIMARY KEY, org_id TEXT, account_id TEXT, status TEXT);
        ").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.pragma_update(None, "foreign_keys", 1).unwrap();
        c
    }

    fn publisher(c: &Connection, id: &str, kind: &str, owner: &str, applied_by: &str) {
        c.execute(
            "INSERT INTO publisher_profiles (id, owner_kind, owner_id, display_name, status, applied_by, created_at, updated_at) VALUES (?1, ?2, ?3, 'name', 'approved', ?4, 0, 0)",
            rusqlite::params![id, kind, owner, applied_by],
        ).unwrap();
    }

    fn release(c: &Connection, id: &str, publisher_id: &str, created_by: &str) {
        c.execute(
            "INSERT INTO nest_templates (id, publisher_id, slug, name, created_by, created_at) VALUES (?1, ?2, ?3, 'name', 'user', 0)",
            rusqlite::params![format!("t_{}", id), publisher_id, format!("slug-{}", id)],
        ).unwrap();
        c.execute(
            "INSERT INTO nest_releases (id, template_id, publisher_id, version, image_digest, status, created_by, created_at) VALUES (?1, ?2, ?3, 'v1', 'sha256:abc', 'draft', ?4, 0)",
            rusqlite::params![id, format!("t_{}", id), publisher_id, created_by],
        ).unwrap();
    }

    #[test]
    fn conflict_when_staff_owns_or_filed() {
        let c = db();

        // Publisher owned by 'staff'
        publisher(&c, "p1", "account", "staff", "other");
        assert!(publisher_conflict_in(&c, "staff", "p1").unwrap());

        // Publisher owned by 'other', applied_by 'staff'
        publisher(&c, "p2", "account", "other", "staff");
        assert!(publisher_conflict_in(&c, "staff", "p2").unwrap());

        // Publisher owned by 'other', applied_by 'other'
        publisher(&c, "p3", "account", "third", "third");
        assert!(!publisher_conflict_in(&c, "staff", "p3").unwrap());
    }

    #[test]
    fn conflict_covers_org_members_and_owner() {
        let c = db();

        // Org publisher
        publisher(&c, "p1", "org", "org1", "other");

        // Active member 'staff' conflicts
        c.execute(
            "INSERT INTO orgs (id, owner_account) VALUES ('org1', 'admin')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO org_members (id, org_id, account_id, status) VALUES ('m1', 'org1', 'staff', 'active')", []).unwrap();
        assert!(publisher_conflict_in(&c, "staff", "p1").unwrap());

        // Suspended member 'staff' conflicts
        c.execute("INSERT INTO org_members (id, org_id, account_id, status) VALUES ('m2', 'org1', 'other_susp', 'suspended')", []).unwrap();
        assert!(publisher_conflict_in(&c, "other_susp", "p1").unwrap());

        // Removed member 'staff' does not conflict (check with a new publisher/org setup to avoid unique index issues or just verify logic)
        // Let's add another org for clean test
        publisher(&c, "p2", "org", "org2", "other");
        c.execute(
            "INSERT INTO orgs (id, owner_account) VALUES ('org2', 'admin')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO org_members (id, org_id, account_id, status) VALUES ('m3', 'org2', 'staff', 'removed')", []).unwrap();
        assert!(!publisher_conflict_in(&c, "staff", "p2").unwrap());

        // Org owner 'staff' conflicts even with no member row
        publisher(&c, "p3", "org", "org3", "other");
        c.execute(
            "INSERT INTO orgs (id, owner_account) VALUES ('org3', 'staff')",
            [],
        )
        .unwrap();
        assert!(publisher_conflict_in(&c, "staff", "p3").unwrap());
    }

    #[test]
    fn unknown_publisher_or_release_counts_as_conflict() {
        let c = db();

        // Unknown publisher
        assert!(publisher_conflict_in(&c, "staff", "nonexistent_pub").unwrap());

        // Unknown release
        assert!(release_conflict_in(&c, "staff", "nonexistent_rel").unwrap());
    }

    #[test]
    fn release_author_conflicts() {
        let c = db();

        // Publisher unrelated to staff
        publisher(&c, "p1", "account", "other", "other");

        // Release created by 'staff'
        release(&c, "r1", "p1", "staff");
        assert!(release_conflict_in(&c, "staff", "r1").unwrap());

        // Release created by 'another_author'
        release(&c, "r2", "p1", "another_author");
        assert!(!release_conflict_in(&c, "staff", "r2").unwrap());
    }

    #[test]
    fn release_digest_is_immutable() {
        let c = db();
        publisher(&c, "p1", "account", "other", "other");
        release(&c, "r1", "p1", "author");

        // Update digest should fail
        let res = c.execute(
            "UPDATE nest_releases SET image_digest='sha256:other' WHERE id='r1'",
            [],
        );
        assert!(res.is_err());

        // Update status should succeed
        let res = c.execute(
            "UPDATE nest_releases SET status='approved' WHERE id='r1'",
            [],
        );
        assert!(res.is_ok());
    }

    #[test]
    fn decisions_and_attestations_are_append_only() {
        let c = db();

        // Setup for review_decisions
        publisher(&c, "p1", "account", "other", "other");
        release(&c, "r1", "p1", "author");
        c.execute("INSERT INTO review_cases (id, release_id, publisher_id, status, created_at, updated_at) VALUES ('c1', 'r1', 'p1', 'open', 0, 0)", []).unwrap();
        c.execute("INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at) VALUES ('d1', 'c1', 'rev', 'approved', 'ok', 0)", []).unwrap();

        // Update decision fails
        assert!(c
            .execute(
                "UPDATE review_decisions SET reason='changed' WHERE id='d1'",
                []
            )
            .is_err());
        // Delete decision fails
        assert!(c
            .execute("DELETE FROM review_decisions WHERE id='d1'", [])
            .is_err());

        // Setup for release_attestations
        c.execute("INSERT INTO release_attestations (id, release_id, case_id, attester, statement_digest, created_at) VALUES ('a1', 'r1', 'c1', 'att', 'dig', 0)", []).unwrap();

        // Update attestation fails
        assert!(c
            .execute(
                "UPDATE release_attestations SET attester='other' WHERE id='a1'",
                []
            )
            .is_err());
        // Delete attestation fails
        assert!(c
            .execute("DELETE FROM release_attestations WHERE id='a1'", [])
            .is_err());
    }

    #[test]
    fn one_live_role_per_account_and_role() {
        let c = db();

        // Insert first live role
        c.execute(
            "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('sr1', 'acc1', 'reviewer', 'admin', 0)",
            [],
        ).unwrap();

        // Second live role for same account and role fails
        let res = c.execute(
            "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('sr2', 'acc1', 'reviewer', 'admin', 0)",
            [],
        );
        assert!(res.is_err());

        // Revoke first
        c.execute("UPDATE staff_roles SET revoked_at=1 WHERE id='sr1'", [])
            .unwrap();

        // Insert second now succeeds
        let res = c.execute(
            "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('sr2', 'acc1', 'reviewer', 'admin', 0)",
            [],
        );
        assert!(res.is_ok());
    }

    #[test]
    fn digest_must_be_sha256() {
        let c = db();
        publisher(&c, "p1", "account", "other", "other");

        // Insert template first to satisfy FK
        c.execute(
            "INSERT INTO nest_templates (id, publisher_id, slug, name, created_by, created_at) VALUES ('t1', 'p1', 's', 'n', 'u', 0)",
            [],
        ).unwrap();

        // Insert release with bad digest fails
        let res = c.execute(
            "INSERT INTO nest_releases (id, template_id, publisher_id, version, image_digest, status, created_by, created_at) VALUES ('r1', 't1', 'p1', 'v1', 'latest', 'draft', 'u', 0)",
            [],
        );
        assert!(res.is_err());
    }
}
