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

pub(crate) const SCHEMA: &str = "
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

-- `platform` is Pigeonpost's own first-party publisher (M1): made by the build plane, never by an
-- application, and the only kind staff may review while belonging to it.
CREATE TABLE IF NOT EXISTS publisher_profiles (
    id           TEXT PRIMARY KEY,
    owner_kind   TEXT NOT NULL CHECK (owner_kind IN ('account','org','platform')),
    owner_id     TEXT NOT NULL,
    display_name TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('applied','approved','rejected','revoked')),
    applied_by   TEXT NOT NULL,
    decided_by   TEXT,
    decided_at   INTEGER,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    -- M2: the staff reason for the latest certification decision or revocation.
    decision_reason TEXT
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
    -- The build record (M1). `release_digest` covers the source, vendored sources, manifest, SBOM
    -- and image digests; it is what staff attest.
    release_digest  TEXT CHECK (release_digest IS NULL OR release_digest LIKE 'sha256:%'),
    source_digest   TEXT,
    vendor_digest   TEXT,
    manifest_digest TEXT,
    sbom_digest     TEXT,
    sbom            TEXT,
    manifest        TEXT,
    image           TEXT,
    build_isolation TEXT,
    -- Runtime security, separate from the review status: quarantine can be lifted, revocation
    -- cannot.
    security_state  TEXT NOT NULL DEFAULT 'normal'
                    CHECK (security_state IN ('normal','quarantined','revoked')),
    UNIQUE (template_id, version)
);
CREATE INDEX IF NOT EXISTS nest_releases_by_publisher ON nest_releases(publisher_id);
-- A release is what was reviewed: its bytes and identity never change, only its status.
CREATE TRIGGER IF NOT EXISTS nest_releases_immutable BEFORE UPDATE ON nest_releases
WHEN OLD.image_digest <> NEW.image_digest OR OLD.version <> NEW.version
  OR OLD.template_id <> NEW.template_id OR OLD.publisher_id <> NEW.publisher_id
  OR OLD.release_digest IS NOT NEW.release_digest OR OLD.source_digest IS NOT NEW.source_digest
  OR OLD.vendor_digest IS NOT NEW.vendor_digest OR OLD.manifest_digest IS NOT NEW.manifest_digest
  OR OLD.sbom_digest IS NOT NEW.sbom_digest OR OLD.sbom IS NOT NEW.sbom
  OR OLD.manifest IS NOT NEW.manifest OR OLD.image IS NOT NEW.image
BEGIN SELECT RAISE(ABORT, 'nest_releases are immutable'); END;
CREATE TRIGGER IF NOT EXISTS nest_releases_revocation_is_permanent BEFORE UPDATE ON nest_releases
WHEN (OLD.security_state = 'revoked' AND NEW.security_state <> 'revoked')
  OR (OLD.status = 'revoked' AND NEW.status <> 'revoked')
BEGIN SELECT RAISE(ABORT, 'a revoked release stays revoked'); END;

CREATE TABLE IF NOT EXISTS nest_listings (
    id           TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    release_id   TEXT NOT NULL REFERENCES nest_releases(id),
    title        TEXT NOT NULL,
    summary      TEXT NOT NULL,
    visibility   TEXT NOT NULL CHECK (visibility IN ('public','unlisted','private','invite_only','organization')),
    status       TEXT NOT NULL CHECK (status IN ('draft','published','suspended','withdrawn')),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    -- The shared Nest address jobs are sent to (`to` in a job request, M1). Not a mailbox: the
    -- dispatcher resolves it here and nowhere else.
    address      TEXT,
    -- M2. Publisher-editable: `title`, `summary`, `description`, `visibility`, and which attested
    -- release of `template_id` new installs get. Staff-controlled: `suspended` (moderation) with
    -- its reason; a publisher can never leave that state.
    description       TEXT NOT NULL DEFAULT '',
    template_id       TEXT REFERENCES nest_templates(id),
    moderation_reason TEXT,
    published_at      INTEGER,
    -- M2b: the listing's price terms. Launch terms are one-off and monthly; the amount stays zero
    -- until payments land (saas_be offers mirror these fields).
    price_currency    TEXT NOT NULL DEFAULT 'TRY' CHECK (price_currency IN ('TRY','USD','EUR')),
    price_term        TEXT NOT NULL DEFAULT 'one_off' CHECK (price_term IN ('one_off','monthly'))
);
CREATE INDEX IF NOT EXISTS nest_listings_by_publisher ON nest_listings(publisher_id);
CREATE UNIQUE INDEX IF NOT EXISTS nest_listings_address ON nest_listings(address)
    WHERE address IS NOT NULL;

CREATE TABLE IF NOT EXISTS review_cases (
    id           TEXT PRIMARY KEY,
    release_id   TEXT NOT NULL REFERENCES nest_releases(id),
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    status       TEXT NOT NULL CHECK (status IN ('open','blocked','decided','withdrawn')),
    previous_id  TEXT REFERENCES review_cases(id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    -- M2: bumped on every change the publisher can see, so a console can tell a new state from a
    -- replay; and the build request a third-party case came from.
    revision     INTEGER NOT NULL DEFAULT 1,
    build_id     TEXT
);
CREATE INDEX IF NOT EXISTS review_cases_by_release ON review_cases(release_id);

-- One row per review step (M1): the automated stages record `passed`/`failed` (the AI stage only
-- ever `advisory`, so AI never decides), and only the human review records a decision.
CREATE TABLE IF NOT EXISTS review_decisions (
    id         TEXT PRIMARY KEY,
    case_id    TEXT NOT NULL REFERENCES review_cases(id),
    reviewer   TEXT NOT NULL,
    decision   TEXT NOT NULL CHECK (decision IN
                   ('approved','rejected','changes_requested','passed','failed','advisory')),
    reason     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    stage      TEXT NOT NULL DEFAULT 'human_review' CHECK (stage IN
                   ('intake','deterministic_scan','ai_analysis','behavioral_test','human_review','attestation')),
    evidence        TEXT,
    evidence_digest TEXT CHECK (evidence_digest IS NULL OR evidence_digest LIKE 'sha256:%'),
    -- Staff reviewed a first-party (platform) release: the conflict rule's one exception.
    first_party INTEGER NOT NULL DEFAULT 0 CHECK (first_party IN (0, 1)),
    CHECK ((stage = 'human_review') = (decision IN ('approved','rejected','changes_requested'))),
    CHECK ((stage = 'ai_analysis') = (decision = 'advisory')),
    CHECK (first_party = 0 OR stage = 'human_review')
);
CREATE INDEX IF NOT EXISTS review_decisions_by_case ON review_decisions(case_id);
-- `first_party` can only describe a platform publisher's case, whatever the code above it does.
CREATE TRIGGER IF NOT EXISTS review_decisions_first_party_is_platform
BEFORE INSERT ON review_decisions WHEN NEW.first_party = 1 AND NOT EXISTS (
    SELECT 1 FROM review_cases c JOIN publisher_profiles p ON p.id = c.publisher_id
     WHERE c.id = NEW.case_id AND p.owner_kind = 'platform')
BEGIN SELECT RAISE(ABORT, 'first_party is for platform publishers only'); END;
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
    -- M1: the canonical statement itself, the release digest it covers and the staff decision it
    -- records. Admission recomputes `statement_digest` from `statement` before trusting it.
    release_digest   TEXT,
    decision_id      TEXT REFERENCES review_decisions(id),
    statement        TEXT,
    first_party      INTEGER NOT NULL DEFAULT 0 CHECK (first_party IN (0, 1)),
    UNIQUE (release_id, statement_digest)
);
CREATE TRIGGER IF NOT EXISTS release_attestations_no_update BEFORE UPDATE ON release_attestations
BEGIN SELECT RAISE(ABORT, 'release_attestations are append-only'); END;
CREATE TRIGGER IF NOT EXISTS release_attestations_no_delete BEFORE DELETE ON release_attestations
BEGIN SELECT RAISE(ABORT, 'release_attestations are append-only'); END;

-- ---- M1: the reference path ---------------------------------------------------------------------

-- What a listing offers, from its release's manifest. M1 is free: no money moves, so a price other
-- than zero cannot be stored.
CREATE TABLE IF NOT EXISTS nest_operations (
    listing_id        TEXT NOT NULL REFERENCES nest_listings(id),
    operation         TEXT NOT NULL,
    operation_version TEXT NOT NULL,
    input_schema      TEXT NOT NULL,
    price_minor       INTEGER NOT NULL DEFAULT 0 CHECK (price_minor = 0),
    PRIMARY KEY (listing_id, operation, operation_version)
);

-- Server-priced quotes; zero only in M1.
CREATE TABLE IF NOT EXISTS nest_quotes (
    id                TEXT PRIMARY KEY,
    account_id        TEXT NOT NULL,
    listing_id        TEXT NOT NULL REFERENCES nest_listings(id),
    release_id        TEXT NOT NULL REFERENCES nest_releases(id),
    operation         TEXT NOT NULL,
    operation_version TEXT NOT NULL,
    amount_minor      INTEGER NOT NULL CHECK (amount_minor = 0),
    currency          TEXT NOT NULL,
    expires_at        INTEGER NOT NULL,
    created_at        INTEGER NOT NULL
);

-- Single-use references, stored as a hash only. Zero-priced: nothing is reserved.
CREATE TABLE IF NOT EXISTS nest_payment_authorizations (
    ref_sha256   TEXT PRIMARY KEY,
    quote_id     TEXT NOT NULL UNIQUE REFERENCES nest_quotes(id),
    account_id   TEXT NOT NULL,
    amount_minor INTEGER NOT NULL CHECK (amount_minor = 0),
    expires_at   INTEGER NOT NULL,
    consumed_by  TEXT,
    consumed_at  INTEGER,
    created_at   INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS nest_payment_authorizations_single_use
BEFORE UPDATE ON nest_payment_authorizations WHEN OLD.consumed_by IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a payment authorization is used once'); END;

-- Jobs: admission, execution and settlement are separate dimensions (contract `NestJob`).
CREATE TABLE IF NOT EXISTS nest_jobs (
    id                   TEXT PRIMARY KEY,
    account_id           TEXT NOT NULL,
    acting               TEXT,
    buyer_context        TEXT NOT NULL,
    listing_id           TEXT NOT NULL REFERENCES nest_listings(id),
    address              TEXT NOT NULL,
    release_id           TEXT NOT NULL REFERENCES nest_releases(id),
    operation            TEXT NOT NULL,
    operation_version    TEXT NOT NULL,
    quote_id             TEXT NOT NULL UNIQUE REFERENCES nest_quotes(id),
    authorization_sha256 TEXT NOT NULL UNIQUE,
    intent_digest        TEXT NOT NULL,
    payload              TEXT NOT NULL,
    client_trace_id      TEXT,
    admission  TEXT NOT NULL CHECK (admission IN ('pending','authorized','funded','admitted','rejected')),
    execution  TEXT NOT NULL CHECK (execution IN ('not_started','queued','running','waiting_input',
                   'reconciling','succeeded','failed','cancelled','expired','stopped_security')),
    settlement TEXT NOT NULL CHECK (settlement IN ('none','reserved','pending_reconciliation',
                   'settled','released','disputed')),
    revision    INTEGER NOT NULL DEFAULT 1,
    result      TEXT,
    error_code  TEXT,
    worker      TEXT,
    deadline_at INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    -- M2b: the installation whose pinned release and configuration the job runs (an account's or
    -- an org's); null for a first-party listing used without one.
    installation_id TEXT
);
CREATE INDEX IF NOT EXISTS nest_jobs_by_execution ON nest_jobs(execution);
CREATE INDEX IF NOT EXISTS nest_jobs_by_release ON nest_jobs(release_id, execution);
CREATE INDEX IF NOT EXISTS nest_jobs_by_account ON nest_jobs(account_id);

-- Ids and states only, never content.
CREATE TABLE IF NOT EXISTS nest_job_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id     TEXT NOT NULL REFERENCES nest_jobs(id),
    kind       TEXT NOT NULL,
    detail     TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_job_events_by_job ON nest_job_events(job_id);
CREATE TRIGGER IF NOT EXISTS nest_job_events_no_update BEFORE UPDATE ON nest_job_events
BEGIN SELECT RAISE(ABORT, 'nest_job_events are append-only'); END;

-- Required `Idempotency-Key`s, kept for the life of the record they created.
CREATE TABLE IF NOT EXISTS operation_idempotency (
    scope          TEXT NOT NULL,
    key            TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    status         INTEGER NOT NULL,
    response       TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (scope, key)
);

-- ---- M2: the publisher beta -------------------------------------------------------------------

-- Who in an org publisher may act for it, beside the org's owner and admins (who always may).
-- Kept here rather than as new `org_roles` values so the live org table is not rebuilt; a role
-- only counts while the person is an active member of the org (checked at use).
CREATE TABLE IF NOT EXISTS publisher_roles (
    id           TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    account_id   TEXT NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('publisher_admin','publisher_finance')),
    granted_by   TEXT NOT NULL,
    granted_at   INTEGER NOT NULL,
    revoked_by   TEXT,
    revoked_at   INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS publisher_roles_live ON publisher_roles(publisher_id, account_id, role)
    WHERE revoked_at IS NULL;

-- Every certification decision and revocation, by whom and why. Append-only.
CREATE TABLE IF NOT EXISTS publisher_certifications (
    id           TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    decision     TEXT NOT NULL CHECK (decision IN ('approved','rejected','revoked')),
    reason       TEXT NOT NULL,
    decided_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS publisher_certifications_no_update BEFORE UPDATE ON publisher_certifications
BEGIN SELECT RAISE(ABORT, 'publisher_certifications are append-only'); END;
CREATE TRIGGER IF NOT EXISTS publisher_certifications_no_delete BEFORE DELETE ON publisher_certifications
BEGIN SELECT RAISE(ABORT, 'publisher_certifications are append-only'); END;

-- A publisher's request to build a release from source (R-62). The build plane takes queued ones,
-- builds them on its own host and answers with a build record or a failure.
CREATE TABLE IF NOT EXISTS nest_builds (
    id             TEXT PRIMARY KEY,
    publisher_id   TEXT NOT NULL REFERENCES publisher_profiles(id),
    template_id    TEXT NOT NULL REFERENCES nest_templates(id),
    version        TEXT NOT NULL,
    source_repository TEXT NOT NULL,
    source_commit  TEXT NOT NULL,
    status         TEXT NOT NULL CHECK (status IN ('queued','built','failed','cancelled')),
    release_id     TEXT REFERENCES nest_releases(id),
    review_id      TEXT REFERENCES review_cases(id),
    failure_code   TEXT,
    failure_reason TEXT,
    requested_by   TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    -- M2b: the registered source connection the build plane fetches through, if any.
    source_id      TEXT,
    UNIQUE (template_id, version)
);
CREATE INDEX IF NOT EXISTS nest_builds_by_status ON nest_builds(status, created_at);

-- An installation: a buyer's pinned use of a listing (R-51, R-68). The buyer context is the M1
-- one (one worker per listing and account). M2 is free: `funding` is test credits only.
CREATE TABLE IF NOT EXISTS nest_installations (
    id                  TEXT PRIMARY KEY,
    listing_id          TEXT NOT NULL REFERENCES nest_listings(id),
    owner_kind          TEXT NOT NULL CHECK (owner_kind IN ('account','org')),
    owner_id            TEXT NOT NULL,
    buyer_context       TEXT NOT NULL,
    release_id          TEXT NOT NULL REFERENCES nest_releases(id),
    previous_release_id TEXT REFERENCES nest_releases(id),
    config              TEXT NOT NULL DEFAULT '{}',
    config_revision     INTEGER NOT NULL DEFAULT 1,
    status              TEXT NOT NULL CHECK (status IN ('active','uninstalled')),
    funding             TEXT NOT NULL DEFAULT 'test_credits' CHECK (funding IN ('test_credits')),
    installed_by        TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    uninstalled_at      INTEGER,
    -- M2b, org installations only: the org agent mailbox operators are delegated on
    -- (`operate_nest`), as for an org Nest. Null for an account's installation.
    agent_address       TEXT,
    CHECK ((owner_kind = 'org') = (agent_address IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS nest_installations_live
    ON nest_installations(listing_id, owner_kind, owner_id) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS nest_installations_by_owner ON nest_installations(owner_kind, owner_id);

-- Secret configuration values (writeOnly properties), sealed with the vault and bound to their
-- row. Never in `nest_installations.config`, never returned by any route.
CREATE TABLE IF NOT EXISTS nest_installation_secrets (
    installation_id TEXT NOT NULL REFERENCES nest_installations(id),
    name            TEXT NOT NULL,
    sealed          BLOB NOT NULL,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (installation_id, name)
);

-- What happened to an installation: ids and versions only, never configuration values.
CREATE TABLE IF NOT EXISTS nest_installation_events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    installation_id TEXT NOT NULL REFERENCES nest_installations(id),
    kind            TEXT NOT NULL CHECK (kind IN
                        ('installed','configured','updated','rolled_back','uninstalled')),
    from_release    TEXT,
    to_release      TEXT,
    actor           TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_installation_events_by_installation
    ON nest_installation_events(installation_id);
-- Append-only, except that an account erasure may replace the actor with its erased marker.
CREATE TRIGGER IF NOT EXISTS nest_installation_events_no_update BEFORE UPDATE ON nest_installation_events
WHEN NEW.actor NOT LIKE 'erased_%' OR NEW.kind <> OLD.kind OR NEW.installation_id <> OLD.installation_id
  OR NEW.from_release IS NOT OLD.from_release OR NEW.to_release IS NOT OLD.to_release
  OR NEW.created_at <> OLD.created_at
BEGIN SELECT RAISE(ABORT, 'nest_installation_events are append-only'); END;

-- Quarantine, release from quarantine and revocation, by whom and why. Append-only.
CREATE TABLE IF NOT EXISTS release_security_events (
    id         TEXT PRIMARY KEY,
    release_id TEXT NOT NULL REFERENCES nest_releases(id),
    action     TEXT NOT NULL CHECK (action IN ('quarantined','released','revoked')),
    actor      TEXT NOT NULL,
    reason     TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS release_security_events_no_update BEFORE UPDATE ON release_security_events
BEGIN SELECT RAISE(ABORT, 'release_security_events are append-only'); END;
CREATE TRIGGER IF NOT EXISTS release_security_events_no_delete BEFORE DELETE ON release_security_events
BEGIN SELECT RAISE(ABORT, 'release_security_events are append-only'); END;
";

/// Marketplace M2b: visibility grants, source connections and hosted test Nests. Additive tables.
pub(super) const SCHEMA_M2B: &str = "
-- Who may see, install and invoke an `invite_only` or `organization` listing (R-52). An invite
-- names a mailbox address and binds to the account that accepts it; an organization grant names
-- an org, and holds for its active members while the org is live. Each of the three uses is its
-- own switch. Revoked rows stay as the record.
CREATE TABLE IF NOT EXISTS nest_listing_grants (
    id              TEXT PRIMARY KEY,
    listing_id      TEXT NOT NULL REFERENCES nest_listings(id),
    kind            TEXT NOT NULL CHECK (kind IN ('invite','organization')),
    invitee_address TEXT,
    account_id      TEXT,
    org_id          TEXT,
    can_discover    INTEGER NOT NULL CHECK (can_discover IN (0, 1)),
    can_install     INTEGER NOT NULL CHECK (can_install IN (0, 1)),
    can_invoke      INTEGER NOT NULL CHECK (can_invoke IN (0, 1)),
    status          TEXT NOT NULL CHECK (status IN ('pending','active','declined','revoked')),
    created_by      TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    accepted_at     INTEGER,
    revoked_by      TEXT,
    revoked_at      INTEGER,
    CHECK ((kind = 'invite') = (invitee_address IS NOT NULL)),
    CHECK ((kind = 'organization') = (org_id IS NOT NULL)),
    CHECK (status <> 'pending' OR kind = 'invite')
);
CREATE INDEX IF NOT EXISTS nest_listing_grants_by_listing ON nest_listing_grants(listing_id, status);
CREATE INDEX IF NOT EXISTS nest_listing_grants_by_account ON nest_listing_grants(account_id, status);
CREATE INDEX IF NOT EXISTS nest_listing_grants_by_address ON nest_listing_grants(invitee_address, status);
CREATE UNIQUE INDEX IF NOT EXISTS nest_listing_grants_live_invite
    ON nest_listing_grants(listing_id, invitee_address) WHERE kind = 'invite' AND status IN ('pending','active');
CREATE UNIQUE INDEX IF NOT EXISTS nest_listing_grants_live_org
    ON nest_listing_grants(listing_id, org_id) WHERE kind = 'organization' AND status = 'active';

-- A publisher's registered source (R-62): a public https repository, or one the build host reads
-- with a deploy key Pigeonpost generated. The private key is sealed with the vault, bound to its
-- row, handed only to the build plane for a queued build, and wiped when the source is deleted.
CREATE TABLE IF NOT EXISTS publisher_sources (
    id                 TEXT PRIMARY KEY,
    publisher_id       TEXT NOT NULL REFERENCES publisher_profiles(id),
    kind               TEXT NOT NULL CHECK (kind IN ('https','deploy_key')),
    repository         TEXT NOT NULL,
    host               TEXT NOT NULL,
    label              TEXT NOT NULL,
    public_key         TEXT,
    key_fingerprint    TEXT,
    private_key_sealed BLOB,
    status             TEXT NOT NULL CHECK (status IN ('active','deleted')),
    created_by         TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    deleted_at         INTEGER,
    CHECK (kind = 'deploy_key' OR (public_key IS NULL AND private_key_sealed IS NULL)),
    CHECK (status = 'deleted' OR kind = 'https' OR private_key_sealed IS NOT NULL),
    CHECK (status = 'active' OR private_key_sealed IS NULL)
);
CREATE INDEX IF NOT EXISTS publisher_sources_by_publisher ON publisher_sources(publisher_id, status);

-- Hosted test Nests (R-70), on a staging postbox only: a release that passed intake, run in the
-- test pool with synthetic jobs until it expires. Never production data, never a buyer.
CREATE TABLE IF NOT EXISTS nest_test_envs (
    id            TEXT PRIMARY KEY,
    publisher_id  TEXT NOT NULL REFERENCES publisher_profiles(id),
    release_id    TEXT NOT NULL REFERENCES nest_releases(id),
    build_id      TEXT,
    label         TEXT NOT NULL,
    buyer_context TEXT NOT NULL,
    generation    INTEGER NOT NULL DEFAULT 1,
    status        TEXT NOT NULL CHECK (status IN ('active','expired','deleted')),
    created_by    TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    warned_at     INTEGER,
    ended_at      INTEGER
);
CREATE INDEX IF NOT EXISTS nest_test_envs_by_publisher ON nest_test_envs(publisher_id, status);
CREATE INDEX IF NOT EXISTS nest_test_envs_by_expiry ON nest_test_envs(status, expires_at);

-- A person's report about a listing (M2b), for staff moderation. One open report per reporter
-- and listing; staff close it with a note.
CREATE TABLE IF NOT EXISTS nest_listing_reports (
    id          TEXT PRIMARY KEY,
    listing_id  TEXT NOT NULL REFERENCES nest_listings(id),
    reporter    TEXT NOT NULL,
    reason      TEXT NOT NULL CHECK (reason IN ('malicious','misleading','broken','privacy','illegal','spam','other')),
    detail      TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL CHECK (status IN ('open','closed')),
    created_at  INTEGER NOT NULL,
    closed_by   TEXT,
    closed_at   INTEGER,
    resolution  TEXT
);
CREATE INDEX IF NOT EXISTS nest_listing_reports_by_status ON nest_listing_reports(status, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS nest_listing_reports_open
    ON nest_listing_reports(listing_id, reporter) WHERE status = 'open';

-- Synthetic jobs on a test Nest. Their payloads are the publisher's own test data.
CREATE TABLE IF NOT EXISTS nest_test_jobs (
    id                TEXT PRIMARY KEY,
    test_nest_id      TEXT NOT NULL REFERENCES nest_test_envs(id),
    generation        INTEGER NOT NULL,
    operation         TEXT NOT NULL,
    operation_version TEXT NOT NULL,
    payload           TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','stopped')),
    result            TEXT,
    error_code        TEXT,
    worker            TEXT,
    created_by        TEXT NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_test_jobs_by_env ON nest_test_jobs(test_nest_id, created_at);

-- What happened to a test Nest: ids, states and error codes only. Its log for the publisher.
CREATE TABLE IF NOT EXISTS nest_test_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    test_nest_id TEXT NOT NULL REFERENCES nest_test_envs(id),
    kind         TEXT NOT NULL,
    detail       TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_test_events_by_env ON nest_test_events(test_nest_id, id);
";

/// Who has a stake in a release, for a staff review. `first_party` is set when the publisher is
/// Pigeonpost's own (`owner_kind = 'platform'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReviewConflict {
    pub conflicted: bool,
    pub first_party: bool,
}

/// The conflict check for a staff review of a release (owner decision 2026-11-10): the M0 rule —
/// a reviewer who owns, filed or belongs to the publisher, or built the release, may not review it
/// — with one exception. A **first-party** release (publisher `owner_kind = 'platform'`) may be
/// reviewed by staff, since every staff member belongs to Pigeonpost and the owner is the only
/// reviewer. Having built the release still conflicts, first party or not. Every third-party
/// publisher keeps the full rule. A release or publisher that cannot be found is a conflict.
pub(crate) fn review_conflict_in(
    c: &Connection,
    staff: &str,
    release: &str,
) -> rusqlite::Result<ReviewConflict> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT r.created_by, p.owner_kind FROM nest_releases r
               JOIN publisher_profiles p ON p.id = r.publisher_id WHERE r.id = ?1",
            [release],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((author, kind)) = row else {
        return Ok(ReviewConflict {
            conflicted: true,
            first_party: false,
        });
    };
    if kind == "platform" {
        return Ok(ReviewConflict {
            conflicted: author == staff,
            first_party: true,
        });
    }
    Ok(ReviewConflict {
        conflicted: release_conflict_in(c, staff, release)?,
        first_party: false,
    })
}

/// The roles `staff_roles.role` admits after [`migrate_staff_roles`]. The original table (above,
/// kept as written so an older binary's schema text is unchanged) admits only `reviewer` and
/// `finance`; SQLite cannot widen a CHECK in place, so the table is rebuilt once.
const STAFF_ROLES_WIDE: &str = "
CREATE TABLE staff_roles_wide (
    id         TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('owner','support','finance','reviewer','ops')),
    granted_by TEXT NOT NULL,
    granted_at INTEGER NOT NULL,
    revoked_by TEXT,
    revoked_at INTEGER
);
";

/// Widen `staff_roles.role` to the five back office roles (2026-11-11), preserving every row.
///
/// Guarded by the table's own definition in `sqlite_master` (a CHECK is not visible through
/// `PRAGMA table_info`), checked again under `BEGIN IMMEDIATE`: a table that already admits `'ops'` is left alone, so this runs once per
/// database and is a no-op afterwards. The rebuild is SQLite's documented procedure for a
/// constraint change: create the new table, copy, drop the old one, rename, recreate the index;
/// all in one transaction, which commits only if the copy holds exactly the old row count. No
/// other table, trigger or view refers to `staff_roles`. An older binary reading the rebuilt table
/// ignores the roles it cannot parse (`live_staff_roles_in`), so a rollback grants nothing new.
pub(crate) fn migrate_staff_roles(c: &Connection) -> rusqlite::Result<()> {
    // `None`: no table yet. `Some(true)`: already wide.
    fn widened(c: &Connection) -> rusqlite::Result<Option<bool>> {
        let sql: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'staff_roles'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(sql.map(|s| s.contains("'ops'")))
    }
    if widened(c)? != Some(false) {
        return Ok(());
    }
    // The write lock first, then the check again: another process opening the same file (the
    // reaper) may have rebuilt the table between the look above and here.
    let tx = rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
    if widened(&tx)? != Some(false) {
        return Ok(());
    }
    let before: i64 = tx.query_row("SELECT COUNT(*) FROM staff_roles", [], |r| r.get(0))?;
    tx.execute_batch("DROP TABLE IF EXISTS staff_roles_wide;")?;
    tx.execute_batch(STAFF_ROLES_WIDE)?;
    tx.execute_batch(
        "INSERT INTO staff_roles_wide (id, account_id, role, granted_by, granted_at, revoked_by, revoked_at)
             SELECT id, account_id, role, granted_by, granted_at, revoked_by, revoked_at FROM staff_roles;
         DROP TABLE staff_roles;
         ALTER TABLE staff_roles_wide RENAME TO staff_roles;
         CREATE UNIQUE INDEX IF NOT EXISTS staff_roles_live ON staff_roles(account_id, role)
             WHERE revoked_at IS NULL;",
    )?;
    let after: i64 = tx.query_row("SELECT COUNT(*) FROM staff_roles", [], |r| r.get(0))?;
    if after != before {
        // Dropping `tx` rolls the whole rebuild back.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
            Some("staff_roles rebuild did not copy every row".into()),
        ));
    }
    tx.commit()
}

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

/// The accounts that act for a publisher and hear its events (M2): the owning account; for an org,
/// its owner, its active full admins and its active members holding a live publisher role.
pub(crate) fn publisher_accounts_in(
    c: &Connection,
    publisher: &str,
) -> rusqlite::Result<Vec<String>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT owner_kind, owner_id FROM publisher_profiles WHERE id = ?1",
            [publisher],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((kind, owner)) = row else {
        return Ok(Vec::new());
    };
    match kind.as_str() {
        "account" => Ok(vec![owner]),
        "org" => {
            let mut stmt = c.prepare(
                "SELECT owner_account FROM orgs WHERE id = ?1
                 UNION
                 SELECT m.account_id FROM org_members m JOIN org_roles r
                     ON r.org_id = m.org_id AND r.member_id = m.id
                  WHERE m.org_id = ?1 AND m.status = 'active' AND m.account_id IS NOT NULL
                    AND r.role IN ('owner','admin')
                 UNION
                 SELECT p.account_id FROM publisher_roles p JOIN org_members m
                     ON m.org_id = ?1 AND m.account_id = p.account_id AND m.status = 'active'
                  WHERE p.publisher_id = ?2 AND p.revoked_at IS NULL",
            )?;
            let rows = stmt
                .query_map(params![owner, publisher], |r| r.get::<_, Option<String>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().flatten().collect())
        }
        _ => Ok(Vec::new()),
    }
}

/// Announce a publisher-side event on `/v1/events` (typed events, ids and states only) to every
/// account that acts for the publisher, in the caller's transaction. Returns the accounts to wake
/// once it commits.
pub(crate) fn publisher_event_in(
    c: &Connection,
    publisher: &str,
    kind: &str,
    data: &serde_json::Value,
    now: u64,
) -> Result<Vec<String>, super::StoreError> {
    let accounts = publisher_accounts_in(c, publisher)?;
    for a in &accounts {
        super::events::append_in(c, a, kind, None, data, now)?;
    }
    // M2b: an org publisher's events also reach the org's webhooks (ids and states only).
    let org: Option<String> = c
        .query_row(
            "SELECT owner_id FROM publisher_profiles WHERE id = ?1 AND owner_kind = 'org'",
            [publisher],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(org) = org {
        let mut body = data.clone();
        if let Some(o) = body.as_object_mut() {
            o.insert("publisher_id".into(), serde_json::json!(publisher));
        }
        org_webhook_in(c, &org, kind, &body, now)?;
    }
    Ok(accounts)
}

/// A marketplace event on one account's `/v1/events` (M2b invites), in the caller's transaction.
pub(crate) fn account_event_in(
    c: &Connection,
    account: &str,
    kind: &str,
    data: &serde_json::Value,
    now: u64,
) -> Result<(), super::StoreError> {
    super::events::append_in(c, account, kind, None, data, now)?;
    Ok(())
}

/// Record a marketplace event for an org's webhooks (`webhooks::EVENTS`), in the caller's
/// transaction, when the org has a live webhook. The worker fans it out like the trigger-written
/// org events. `data` is ids and states only.
pub(crate) fn org_webhook_in(
    c: &Connection,
    org: &str,
    kind: &str,
    data: &serde_json::Value,
    now: u64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO webhook_events (id, org_id, type, data, created_at)
         SELECT 'evt_' || lower(hex(randomblob(16))), ?1, ?2, ?3, ?4
          WHERE EXISTS (SELECT 1 FROM webhooks WHERE org_id = ?1 AND deleted_at IS NULL)",
        params![org, kind, data.to_string(), now as i64],
    )?;
    Ok(())
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

    /// The owner decision of 2026-11-10, both sides: staff may review a first-party (platform)
    /// release, and every third-party publisher keeps the M0 conflict rule.
    #[test]
    fn first_party_releases_may_be_reviewed_by_staff_and_third_parties_keep_the_rule() {
        let c = db();
        publisher(&c, "pplat", "platform", "pigeonpost", "system:build-plane");
        release(&c, "rplat", "pplat", "system:build-plane");
        assert_eq!(
            review_conflict_in(&c, "owner", "rplat").unwrap(),
            ReviewConflict {
                conflicted: false,
                first_party: true
            }
        );
        // Having built a release conflicts, first party or not.
        release(&c, "rplat2", "pplat", "owner");
        assert!(
            review_conflict_in(&c, "owner", "rplat2")
                .unwrap()
                .conflicted
        );

        // Third party: owning, filing, belonging to or owning the org all conflict, and none of
        // them is ever first party.
        publisher(&c, "pacct", "account", "owner", "owner");
        release(&c, "racct", "pacct", "author");
        publisher(&c, "pfiled", "account", "someone", "owner");
        release(&c, "rfiled", "pfiled", "author");
        c.execute_batch(
            "INSERT INTO orgs (id, owner_account) VALUES ('o1', 'boss');
             INSERT INTO org_members (id, org_id, account_id, status) VALUES ('m1', 'o1', 'owner', 'active');",
        )
        .unwrap();
        publisher(&c, "porg", "org", "o1", "boss");
        release(&c, "rorg", "porg", "author");
        for (staff, rel) in [
            ("owner", "racct"),
            ("owner", "rfiled"),
            ("owner", "rorg"),
            ("boss", "rorg"),
        ] {
            assert_eq!(
                review_conflict_in(&c, staff, rel).unwrap(),
                ReviewConflict {
                    conflicted: true,
                    first_party: false
                },
                "{staff} on {rel}"
            );
        }
        // An unrelated third party is clear, and still not first party.
        publisher(&c, "pfree", "account", "third", "third");
        release(&c, "rfree", "pfree", "author");
        assert_eq!(
            review_conflict_in(&c, "owner", "rfree").unwrap(),
            ReviewConflict {
                conflicted: false,
                first_party: false
            }
        );
        // A release that does not exist is a conflict.
        assert!(review_conflict_in(&c, "owner", "rnone").unwrap().conflicted);
    }

    #[test]
    fn the_schema_keeps_first_party_and_ai_decisions_honest() {
        let c = db();
        publisher(&c, "pplat", "platform", "pigeonpost", "system:build-plane");
        release(&c, "rplat", "pplat", "system:build-plane");
        publisher(&c, "pacct", "account", "other", "other");
        release(&c, "racct", "pacct", "author");
        c.execute_batch(
            "INSERT INTO review_cases (id, release_id, publisher_id, status, created_at, updated_at) VALUES ('cplat', 'rplat', 'pplat', 'open', 0, 0);
             INSERT INTO review_cases (id, release_id, publisher_id, status, created_at, updated_at) VALUES ('cacct', 'racct', 'pacct', 'open', 0, 0);",
        )
        .unwrap();
        let insert = |id: &str, case: &str, stage: &str, decision: &str, first_party: i64| {
            c.execute(
                "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at, stage, first_party)
                 VALUES (?1, ?2, 'rev', ?4, 'r', 0, ?3, ?5)",
                rusqlite::params![id, case, stage, decision, first_party],
            )
        };
        // first_party only on a platform case, and only on the human review.
        assert!(insert("d1", "cacct", "human_review", "approved", 1).is_err());
        assert!(insert("d2", "cplat", "deterministic_scan", "passed", 1).is_err());
        assert!(insert("d3", "cplat", "human_review", "approved", 1).is_ok());
        // AI is advisory only; it can neither pass, fail nor approve.
        for decision in ["approved", "passed", "failed"] {
            assert!(
                insert("d4", "cacct", "ai_analysis", decision, 0).is_err(),
                "{decision}"
            );
        }
        assert!(insert("d5", "cacct", "ai_analysis", "advisory", 0).is_ok());
        // Automated stages never carry a human decision, and the human review never "passes".
        assert!(insert("d6", "cacct", "behavioral_test", "approved", 0).is_err());
        assert!(insert("d7", "cacct", "human_review", "passed", 0).is_err());
        // A revoked release stays revoked; a quarantine can be lifted.
        c.execute(
            "UPDATE nest_releases SET security_state = 'quarantined' WHERE id = 'racct'",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE nest_releases SET security_state = 'normal' WHERE id = 'racct'",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE nest_releases SET security_state = 'revoked' WHERE id = 'racct'",
            [],
        )
        .unwrap();
        assert!(c
            .execute(
                "UPDATE nest_releases SET security_state = 'normal' WHERE id = 'racct'",
                []
            )
            .is_err());
        // Zero only: no price and no quote can carry money in M1.
        c.execute_batch(
            "INSERT INTO nest_listings (id, publisher_id, release_id, title, summary, visibility, status, created_at, updated_at)
             VALUES ('l1', 'pacct', 'racct', 't', 's', 'public', 'published', 0, 0);",
        )
        .unwrap();
        assert!(c
            .execute(
                "INSERT INTO nest_operations (listing_id, operation, operation_version, input_schema, price_minor)
                 VALUES ('l1', 'a.b', '1.0', '{}', 100)",
                []
            )
            .is_err());
        assert!(c
            .execute(
                "INSERT INTO nest_quotes (id, account_id, listing_id, release_id, operation, operation_version, amount_minor, currency, expires_at, created_at)
                 VALUES ('q1', 'a', 'l1', 'racct', 'a.b', '1.0', 1, 'TRY', 0, 0)",
                []
            )
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
