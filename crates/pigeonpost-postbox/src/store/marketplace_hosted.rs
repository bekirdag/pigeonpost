//! Publisher-hosted listings (plan M6, pulled forward for one listing kind).
//!
//! The marketplace tables are live, so nothing here changes an existing `CREATE TABLE`: new
//! columns are added with `ALTER TABLE … ADD COLUMN`, each guarded by `pragma table_info`, and the
//! two job tables are new (`CREATE TABLE IF NOT EXISTS`).
//!
//! No payment is stored here. Paid third-party jobs will be funded by prepaid credits, designed
//! separately; until then a priced publisher-hosted listing takes no jobs.

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
    for (table, column, def) in COLUMNS {
        if !has_column(c, table, column)? {
            c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def}"))?;
        }
    }
    c.execute_batch(SCHEMA)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

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
