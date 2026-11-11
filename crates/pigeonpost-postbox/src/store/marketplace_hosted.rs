//! Publisher-hosted listings (plan M6, pulled forward for one listing kind).
//!
//! The marketplace tables are live, so nothing here changes an existing `CREATE TABLE`: new
//! columns are added with `ALTER TABLE … ADD COLUMN`, each guarded by `pragma table_info`, and the
//! two job tables are new (`CREATE TABLE IF NOT EXISTS`).
//!
//! Credits v1 (`marketplace_credits`) adds three new tables: a per-job USD price per operation,
//! the immutable `per_job` offers mirrored to saas_be for those prices, and one charge row per paid
//! job (the hold, then its capture or release, retried like an outbox). The money itself (balances,
//! holds, sales) lives in saas_be; these rows only say what the postbox asked for and what it heard.

use rusqlite::Connection;

/// The new tables. Hosted jobs never enter `nest_jobs`: that table's quotes and authorizations
/// are zero-priced by CHECK, and its dispatcher hands jobs to nestd, which a publisher-hosted
/// listing never reaches.
pub(crate) const SCHEMA: &str = "
-- One buyer's job on a publisher-hosted listing: admitted, dispatched as a Pigeonpost message to
-- the publisher's service, and finished by the service's result. The buyer's identity never
-- leaves this row: the publisher sees the job id only.
CREATE TABLE IF NOT EXISTS nest_hosted_jobs (
    id                TEXT PRIMARY KEY,
    account_id        TEXT NOT NULL,
    listing_id        TEXT NOT NULL REFERENCES nest_listings(id),
    release_id        TEXT NOT NULL REFERENCES nest_releases(id),
    publisher_id      TEXT NOT NULL,
    operation         TEXT NOT NULL,
    operation_version TEXT NOT NULL,
    input             TEXT NOT NULL,
    deliver_to        TEXT NOT NULL,
    service_mailbox   TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed',
                          'expired','cancelled')),
    attempts          INTEGER NOT NULL DEFAULT 0,
    next_attempt_at   INTEGER,
    last_error        TEXT,
    dispatch_message  TEXT,
    delivery          TEXT CHECK (delivery IS NULL OR delivery IN ('pending','delivered','undeliverable')),
    delivery_attempts INTEGER NOT NULL DEFAULT 0,
    delivery_message  TEXT,
    summary           TEXT,
    result_digest     TEXT,
    error_code        TEXT,
    revision          INTEGER NOT NULL DEFAULT 1,
    deadline_at       INTEGER NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    finished_at       INTEGER
);
CREATE INDEX IF NOT EXISTS nest_hosted_jobs_by_account ON nest_hosted_jobs(account_id, created_at);
CREATE INDEX IF NOT EXISTS nest_hosted_jobs_by_state ON nest_hosted_jobs(state, next_attempt_at);

-- The files a hosted job's result carries. The bytes are in the attachment blob store (by
-- sha256); the attachment sweeps keep a blob while a row here names it.
CREATE TABLE IF NOT EXISTS nest_hosted_job_files (
    job_id     TEXT NOT NULL REFERENCES nest_hosted_jobs(id),
    idx        INTEGER NOT NULL,
    name       TEXT NOT NULL,
    media_type TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    sha256     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (job_id, idx)
);
CREATE INDEX IF NOT EXISTS nest_hosted_job_files_by_sha ON nest_hosted_job_files(sha256);

-- Credits v1: what one job of a publisher-hosted operation costs, in USD cents, as its publisher
-- set it. No row: the operation is free. `nest_listing_prices` cannot hold this (its term CHECK is
-- one_off/monthly), so it is a table of its own.
CREATE TABLE IF NOT EXISTS nest_hosted_prices (
    listing_id   TEXT NOT NULL REFERENCES nest_listings(id),
    operation    TEXT NOT NULL,
    amount_cents INTEGER NOT NULL CHECK (amount_cents BETWEEN 1 AND 1000000),
    currency     TEXT NOT NULL DEFAULT 'USD' CHECK (currency = 'USD'),
    updated_by   TEXT NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (listing_id, operation)
);

-- The `per_job` offers mirrored to saas_be for those prices. Immutable there, so a new price (or
-- release line) is a new offer id and the previous one is retired. `pending`: not mirrored yet
-- (`last_error` says why; the worker retries); `active`: holds name it; `retiring`: saas_be has
-- not confirmed the retirement yet (the worker retries); `retired`.
CREATE TABLE IF NOT EXISTS nest_hosted_offers (
    offer_id     TEXT PRIMARY KEY,
    listing_id   TEXT NOT NULL REFERENCES nest_listings(id),
    operation    TEXT NOT NULL,
    publisher_id TEXT NOT NULL,
    release_line TEXT NOT NULL,
    amount_cents INTEGER NOT NULL CHECK (amount_cents > 0),
    state        TEXT NOT NULL CHECK (state IN ('pending','active','retiring','retired')),
    last_error   TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_hosted_offers_by_listing ON nest_hosted_offers(listing_id, operation, state);

-- One paid job's charge, an outbox for saas_be: the hold placed before the job was queued (its
-- idempotency key is the job id), then exactly one capture (a succeeded result delivered) or one
-- release (failed, expired, service mailbox lost, cancelled, or the job never started). The worker
-- retries `capture_pending`/`release_pending` (and an unconfirmed `holding`) until saas_be answers,
-- so a job is never charged twice and a hold never leaks. `refused`: saas_be refused the capture
-- or release for good (staff look at `last_error`). No card data, no balance: saas_be has those.
CREATE TABLE IF NOT EXISTS nest_hosted_charges (
    job_id           TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL,
    listing_id       TEXT NOT NULL,
    offer_id         TEXT NOT NULL,
    amount_cents     INTEGER NOT NULL CHECK (amount_cents > 0),
    hold_id          TEXT,
    hold_expires_at  INTEGER,
    state            TEXT NOT NULL CHECK (state IN ('holding','held','capture_pending','captured',
                         'release_pending','released','refused')),
    release_reason   TEXT,
    sale_id          TEXT,
    commission_cents INTEGER,
    publisher_cents  INTEGER,
    attempts         INTEGER NOT NULL DEFAULT 0,
    next_attempt_at  INTEGER NOT NULL,
    last_error       TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    settled_at       INTEGER
);
CREATE INDEX IF NOT EXISTS nest_hosted_charges_by_state ON nest_hosted_charges(state, next_attempt_at);
CREATE INDEX IF NOT EXISTS nest_hosted_charges_by_account ON nest_hosted_charges(account_id);
";

/// `(table, column, definition)` added to live tables.
const COLUMNS: &[(&str, &str, &str)] = &[
    // Who runs a listing: Pigeonpost (a reviewed, attested image on a Nest) or the publisher (an
    // integration Pigeonpost reviewed; the code is the publisher's).
    (
        "nest_listings",
        "hosting",
        "TEXT NOT NULL DEFAULT 'pigeonpost' CHECK (hosting IN ('pigeonpost','publisher'))",
    ),
    // A publisher-hosted listing's service mailbox, as the integration named it.
    ("nest_listings", "service_address", "TEXT"),
    // When a hosted job's next delivery attempt to the buyer is due (backoff, like dispatch).
    ("nest_hosted_jobs", "delivery_next_at", "INTEGER"),
    // What a review is of: a built release (the M1 stages) or a publisher-hosted integration
    // (publisher and integration review by staff only).
    (
        "review_cases",
        "kind",
        "TEXT NOT NULL DEFAULT 'release' CHECK (kind IN ('release','integration'))",
    ),
];

fn has_column(c: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|n| n == column))
}

/// Run after the marketplace schemas. Idempotent: every column add checks what is stored first.
pub(crate) fn migrate(c: &Connection) -> rusqlite::Result<()> {
    // Columns on tables that exist, then the new tables, then the columns of those new tables
    // (`nest_hosted_jobs.delivery_next_at` came after its table was first created).
    add_columns(c)?;
    c.execute_batch(SCHEMA)?;
    add_columns(c)
}

fn add_columns(c: &Connection) -> rusqlite::Result<()> {
    for (table, column, def) in COLUMNS {
        let exists: bool = c.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )?;
        if exists && !has_column(c, table, column)? {
            c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// A postbox that already has the first version of `nest_hosted_jobs` (no delivery backoff)
    /// gains the column in place, rows kept.
    #[test]
    fn an_existing_hosted_job_table_gains_the_delivery_backoff_column() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE nest_listings (id TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE nest_releases (id TEXT PRIMARY KEY);
             CREATE TABLE review_cases (id TEXT PRIMARY KEY, status TEXT);
             CREATE TABLE nest_hosted_jobs (id TEXT PRIMARY KEY, account_id TEXT, created_at INTEGER,
                 state TEXT, next_attempt_at INTEGER, delivery TEXT);
             INSERT INTO nest_hosted_jobs (id, delivery) VALUES ('job_a', 'pending');",
        )
        .unwrap();
        super::migrate(&c).unwrap();
        let next: Option<i64> = c
            .query_row(
                "SELECT delivery_next_at FROM nest_hosted_jobs WHERE id = 'job_a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(next, None);
    }

    #[test]
    fn the_migration_upgrades_live_tables_in_place_and_keeps_their_rows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE nest_listings (id TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE nest_releases (id TEXT PRIMARY KEY);
             CREATE TABLE review_cases (id TEXT PRIMARY KEY, status TEXT);
             INSERT INTO nest_listings VALUES ('lst_a', 'A');
             INSERT INTO review_cases VALUES ('rvw_a', 'open');",
        )
        .unwrap();
        super::migrate(&c).unwrap();
        super::migrate(&c).unwrap();
        assert!(super::has_column(&c, "nest_hosted_jobs", "delivery_next_at").unwrap());
        let hosting: String = c
            .query_row(
                "SELECT hosting FROM nest_listings WHERE id = 'lst_a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hosting, "pigeonpost");
        let kind: String = c
            .query_row(
                "SELECT kind FROM review_cases WHERE id = 'rvw_a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "release");
        assert!(c
            .execute(
                "UPDATE nest_listings SET hosting = 'cloud' WHERE id = 'lst_a'",
                []
            )
            .is_err());
        assert!(c
            .execute(
                "UPDATE review_cases SET kind = 'other' WHERE id = 'rvw_a'",
                []
            )
            .is_err());
    }
}
