//! The durable ledger: Nests, runtime runs, leases, broker tokens, token reservations and usage.
//!
//! SQLite in WAL mode, shared by the control service (root) and the broker (its own user) through
//! one file. Phase A runs on one host with one writer per row, so SQLite gives atomic reservations
//! (`BEGIN IMMEDIATE`) without operating a database server; the schema is plain SQL so it moves to
//! PostgreSQL when the control plane is split from the workers (docs/nests/phase-a-plan.md §5).

use crate::util::{now, period_of, random_hex};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nests (
    id             TEXT PRIMARY KEY,
    account_id     TEXT NOT NULL,
    address        TEXT NOT NULL,
    label          TEXT NOT NULL,
    state          TEXT NOT NULL,
    desired        TEXT NOT NULL,
    stop_reason    TEXT,
    generation     INTEGER NOT NULL DEFAULT 0,
    quota_gib      INTEGER NOT NULL,
    input_limit    INTEGER NOT NULL,
    output_limit   INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    started_at     INTEGER,
    last_active_at INTEGER,
    last_error     TEXT
);
CREATE TABLE IF NOT EXISTS broker_tokens (
    nest_id      TEXT PRIMARY KEY REFERENCES nests(id),
    token_sha256 TEXT NOT NULL UNIQUE,
    generation   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS leases (
    nest_id     TEXT PRIMARY KEY REFERENCES nests(id),
    holder      TEXT NOT NULL,
    generation  INTEGER NOT NULL,
    acquired_at INTEGER NOT NULL,
    renewed_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS runs (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    nest_id    TEXT NOT NULL REFERENCES nests(id),
    generation INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    start_ms   INTEGER,
    ended_at   INTEGER,
    end_reason TEXT
);
CREATE INDEX IF NOT EXISTS runs_open ON runs(nest_id, ended_at);
CREATE TABLE IF NOT EXISTS usage (
    nest_id         TEXT NOT NULL REFERENCES nests(id),
    period          TEXT NOT NULL,
    input_used      INTEGER NOT NULL DEFAULT 0,
    output_used     INTEGER NOT NULL DEFAULT 0,
    input_reserved  INTEGER NOT NULL DEFAULT 0,
    output_reserved INTEGER NOT NULL DEFAULT 0,
    platform_input  INTEGER NOT NULL DEFAULT 0,
    platform_output INTEGER NOT NULL DEFAULT 0,
    requests        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (nest_id, period)
);
CREATE TABLE IF NOT EXISTS reservations (
    id              TEXT PRIMARY KEY,
    nest_id         TEXT NOT NULL REFERENCES nests(id),
    period          TEXT NOT NULL,
    generation      INTEGER NOT NULL,
    input_reserved  INTEGER NOT NULL,
    output_reserved INTEGER NOT NULL,
    model           TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('open','settled')),
    outcome         TEXT,
    created_at      INTEGER NOT NULL,
    settled_at      INTEGER,
    input_actual    INTEGER,
    output_actual   INTEGER,
    upstream_id     TEXT
);
CREATE INDEX IF NOT EXISTS reservations_open ON reservations(nest_id, state);
CREATE TABLE IF NOT EXISTS events (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    nest_id TEXT,
    at      INTEGER NOT NULL,
    kind    TEXT NOT NULL,
    detail  TEXT
);
-- Marketplace pool (build plan M1): attested releases this host may run, one worker per listing
-- and buyer context, and the jobs handed to them. A worker also has a `nests` row, so the broker
-- meters it like any Nest; the first-party lifecycle (reconcile, idle stop, list) skips it.
CREATE TABLE IF NOT EXISTS mp_releases (
    release_id       TEXT PRIMARY KEY,
    image            TEXT NOT NULL,
    release_digest   TEXT NOT NULL,
    statement_digest TEXT NOT NULL,
    state            TEXT NOT NULL CHECK (state IN ('admitted','quarantined','revoked')),
    updated_at       INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS mp_releases_revocation_is_permanent BEFORE UPDATE ON mp_releases
WHEN OLD.state = 'revoked' AND NEW.state <> 'revoked'
BEGIN SELECT RAISE(ABORT, 'a revoked release stays revoked'); END;
CREATE TRIGGER IF NOT EXISTS mp_releases_identity BEFORE UPDATE ON mp_releases
WHEN OLD.image <> NEW.image OR OLD.release_digest <> NEW.release_digest
BEGIN SELECT RAISE(ABORT, 'a release never changes its image'); END;
CREATE TABLE IF NOT EXISTS mp_workers (
    nest_id       TEXT PRIMARY KEY REFERENCES nests(id),
    listing_id    TEXT NOT NULL,
    buyer_context TEXT NOT NULL,
    release_id    TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    UNIQUE (listing_id, buyer_context)
);
CREATE INDEX IF NOT EXISTS mp_workers_by_release ON mp_workers(release_id);
CREATE TABLE IF NOT EXISTS mp_jobs (
    job_id     TEXT PRIMARY KEY,
    nest_id    TEXT NOT NULL REFERENCES mp_workers(nest_id),
    state      TEXT NOT NULL CHECK (state IN ('running','succeeded','failed','stopped_security')),
    result     TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
-- Hardening (W1): listings the platform stopped (the kill switch), kept until it restores them,
-- and which publisher each worker serves, for the per-publisher limits.
CREATE TABLE IF NOT EXISTS mp_listing_stops (
    listing_id TEXT PRIMARY KEY,
    stopped_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mp_worker_publishers (
    nest_id      TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS mp_worker_publishers_by_publisher
    ON mp_worker_publishers(publisher_id);
";

/// How long a control instance's claim on a Nest lasts without renewal.
pub const LEASE_TTL_SECS: i64 = 180;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Nest {
    pub id: String,
    pub account_id: String,
    pub address: String,
    pub label: String,
    pub state: String,
    pub desired: String,
    pub stop_reason: Option<String>,
    pub generation: i64,
    pub quota_gib: i64,
    pub input_limit: i64,
    pub output_limit: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub last_active_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct Usage {
    pub period: String,
    pub input_used: i64,
    pub output_used: i64,
    pub input_reserved: i64,
    pub output_reserved: i64,
    pub platform_input: i64,
    pub platform_output: i64,
    pub requests: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: String,
    pub nest_id: String,
    pub period: String,
    pub input_reserved: i64,
    pub output_reserved: i64,
}

/// Why the broker refused to reserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveError {
    /// Unknown, revoked or stale (previous generation) token.
    Unauthorized,
    /// The Nest exists but is not running (paused, stopping, deleted).
    NotRunning,
    /// Another request from this Nest is in flight (one at a time in Phase A).
    Busy,
    /// The monthly input or output allowance cannot cover this request's reservation.
    Exhausted(&'static str),
    /// This Nest has no model allowance at all (an Empty Nest package: no AI connected).
    NoModel,
    /// The operator's host-wide monthly cap (all Nests together) cannot cover this reservation.
    PlatformCap(&'static str),
    Store(String),
}

/// Operator spend caps from `nestd.toml` `[broker]`, applied on top of each Nest's own allowance.
/// `None` means no cap. All are monthly token counts in the same period as the allowances.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    /// Ceiling on any one Nest's monthly allowance, whatever the postbox granted it.
    pub nest_input: Option<i64>,
    pub nest_output: Option<i64>,
    /// Ceiling on all Nests together this month: used + reserved + platform (unknown-outcome) tokens.
    pub global_input: Option<i64>,
    pub global_output: Option<i64>,
}

/// How a reserved request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Upstream answered with usage: charge the actual tokens to the Nest.
    Ok,
    /// Never reached the upstream (refused before sending, connection refused): charge nothing.
    NotSent,
    /// Sent, but the result is unknown (timeout, dropped connection, missing usage). The supplier
    /// may have billed it; the whole reservation is recorded against the platform's error budget,
    /// not the customer's allowance, and is never retried blindly.
    Unknown,
}

fn store_err(e: rusqlite::Error) -> String {
    e.to_string()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(store_err)?;
        Self::init(conn)
    }

    pub fn open_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(store_err)?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(store_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(store_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(store_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(store_err)?;
        conn.execute_batch(SCHEMA).map_err(store_err)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let mut guard = self.conn.lock().map_err(|_| "ledger lock poisoned")?;
        f(&mut guard).map_err(store_err)
    }

    pub fn event(&self, nest_id: Option<&str>, kind: &str, detail: &str) {
        let _ = self.with(|c| {
            c.execute(
                "INSERT INTO events (nest_id, at, kind, detail) VALUES (?1, ?2, ?3, ?4)",
                params![nest_id, now(), kind, detail],
            )
        });
    }

    // ---- Nests ---------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn insert_nest(
        &self,
        id: &str,
        account_id: &str,
        address: &str,
        label: &str,
        quota_gib: i64,
        input_limit: i64,
        output_limit: i64,
    ) -> Result<(), String> {
        let t = now();
        self.with(|c| {
            c.execute(
                "INSERT INTO nests (id, account_id, address, label, state, desired, quota_gib,
                    input_limit, output_limit, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'provisioning', 'running', ?5, ?6, ?7, ?8, ?8)",
                params![
                    id,
                    account_id,
                    address,
                    label,
                    quota_gib,
                    input_limit,
                    output_limit,
                    t
                ],
            )
            .map(|_| ())
        })
    }

    pub fn nest(&self, id: &str) -> Result<Option<Nest>, String> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {NEST_COLS} FROM nests WHERE id = ?1"),
                params![id],
                row_nest,
            )
            .optional()
        })
    }

    pub fn nests(&self, include_deleted: bool) -> Result<Vec<Nest>, String> {
        self.with(|c| {
            let sql = if include_deleted {
                format!("SELECT {NEST_COLS} FROM nests ORDER BY created_at")
            } else {
                format!(
                    "SELECT {NEST_COLS} FROM nests WHERE state != 'deleted' ORDER BY created_at"
                )
            };
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], row_nest)?;
            rows.collect()
        })
    }

    pub fn set_state(
        &self,
        id: &str,
        state: &str,
        desired: &str,
        stop_reason: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE nests SET state = ?2, desired = ?3, stop_reason = ?4,
                    last_error = COALESCE(?5, last_error), updated_at = ?6 WHERE id = ?1",
                params![id, state, desired, stop_reason, error, now()],
            )
            .map(|_| ())
        })
    }

    /// A new runtime generation: fences every credential and lease of the previous one.
    pub fn next_generation(&self, id: &str) -> Result<i64, String> {
        self.with(|c| {
            c.query_row(
                "UPDATE nests SET generation = generation + 1, started_at = ?2, last_active_at = ?2,
                    updated_at = ?2 WHERE id = ?1 RETURNING generation",
                params![id, now()],
                |r| r.get(0),
            )
        })
    }

    pub fn touch_active(&self, id: &str) {
        let _ = self.with(|c| {
            c.execute(
                "UPDATE nests SET last_active_at = ?2 WHERE id = ?1",
                params![id, now()],
            )
        });
    }

    // ---- Broker tokens -------------------------------------------------------------------------

    /// Install the token for a generation, replacing any earlier one (which stops working).
    pub fn set_broker_token(
        &self,
        nest_id: &str,
        token_sha256: &str,
        generation: i64,
    ) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO broker_tokens (nest_id, token_sha256, generation, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(nest_id) DO UPDATE SET token_sha256 = excluded.token_sha256,
                    generation = excluded.generation, created_at = excluded.created_at",
                params![nest_id, token_sha256, generation, now()],
            )
            .map(|_| ())
        })
    }

    /// The Nest a live (current-generation) broker token belongs to.
    pub fn nest_for_token(&self, token_sha256: &str) -> Result<Option<Nest>, String> {
        self.with(|c| {
            c.query_row(
                &format!(
                    "SELECT {} FROM broker_tokens t JOIN nests n ON n.id = t.nest_id
                     WHERE t.token_sha256 = ?1 AND t.generation = n.generation",
                    NEST_COLS
                        .split(',')
                        .map(|c| format!("n.{}", c.trim()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                params![token_sha256],
                row_nest,
            )
            .optional()
        })
    }

    pub fn revoke_broker_token(&self, nest_id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "DELETE FROM broker_tokens WHERE nest_id = ?1",
                params![nest_id],
            )
            .map(|_| ())
        })
    }

    // ---- Leases and runs -----------------------------------------------------------------------

    /// Claim (or renew) the right to operate a Nest. Fails if another holder's lease is live.
    pub fn acquire_lease(&self, nest_id: &str, holder: &str) -> Result<bool, String> {
        let t = now();
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: Option<(String, i64)> = tx
                .query_row(
                    "SELECT holder, renewed_at FROM leases WHERE nest_id = ?1",
                    params![nest_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let ok = match current {
                Some((h, renewed)) if h != holder && t - renewed < LEASE_TTL_SECS => false,
                Some((h, _)) if h == holder => {
                    tx.execute(
                        "UPDATE leases SET renewed_at = ?2 WHERE nest_id = ?1",
                        params![nest_id, t],
                    )?;
                    true
                }
                _ => {
                    tx.execute(
                        "INSERT INTO leases (nest_id, holder, generation, acquired_at, renewed_at)
                         VALUES (?1, ?2, 1, ?3, ?3)
                         ON CONFLICT(nest_id) DO UPDATE SET holder = excluded.holder,
                            generation = leases.generation + 1, acquired_at = excluded.acquired_at,
                            renewed_at = excluded.renewed_at",
                        params![nest_id, holder, t],
                    )?;
                    true
                }
            };
            tx.commit()?;
            Ok(ok)
        })
    }

    pub fn release_lease(&self, nest_id: &str) {
        let _ = self.with(|c| c.execute("DELETE FROM leases WHERE nest_id = ?1", params![nest_id]));
    }

    pub fn open_run(&self, nest_id: &str, generation: i64, start_ms: i64) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO runs (nest_id, generation, started_at, start_ms) VALUES (?1, ?2, ?3, ?4)",
                params![nest_id, generation, now(), start_ms],
            )
            .map(|_| ())
        })
    }

    /// Close any open run of this Nest. Running wall time is `ended_at - started_at` per run.
    pub fn close_runs(&self, nest_id: &str, reason: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE runs SET ended_at = ?2, end_reason = ?3 WHERE nest_id = ?1 AND ended_at IS NULL",
                params![nest_id, now(), reason],
            )
            .map(|_| ())
        })
    }

    /// Running wall time this period, in seconds, counting open runs up to now.
    pub fn running_secs(&self, nest_id: &str, since: i64) -> Result<i64, String> {
        let t = now();
        self.with(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(COALESCE(ended_at, ?3) - MAX(started_at, ?2)), 0) FROM runs
                 WHERE nest_id = ?1 AND COALESCE(ended_at, ?3) > ?2",
                params![nest_id, since, t],
                |r| r.get(0),
            )
        })
    }

    // ---- Usage and reservations ----------------------------------------------------------------

    pub fn usage(&self, nest_id: &str, period: &str) -> Result<Usage, String> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT period, input_used, output_used, input_reserved, output_reserved,
                    platform_input, platform_output, requests FROM usage
                 WHERE nest_id = ?1 AND period = ?2",
                params![nest_id, period],
                |r| {
                    Ok(Usage {
                        period: r.get(0)?,
                        input_used: r.get(1)?,
                        output_used: r.get(2)?,
                        input_reserved: r.get(3)?,
                        output_reserved: r.get(4)?,
                        platform_input: r.get(5)?,
                        platform_output: r.get(6)?,
                        requests: r.get(7)?,
                    })
                },
            )
            .optional()?
            .unwrap_or(Usage {
                period: period.to_string(),
                ..Usage::default()
            }))
        })
    }

    /// Atomically authenticate a broker token and reserve the worst case of one request against
    /// the Nest's allowance for the current period.
    pub fn reserve(
        &self,
        token_sha256: &str,
        input_tokens: i64,
        output_tokens: i64,
        model: &str,
    ) -> Result<Reservation, ReserveError> {
        self.reserve_capped(
            token_sha256,
            input_tokens,
            output_tokens,
            model,
            Caps::default(),
        )
    }

    /// [`Db::reserve`] with the operator's per-Nest and host-wide monthly caps applied in the same
    /// transaction, so concurrent Nests cannot together overdraw the global cap.
    pub fn reserve_capped(
        &self,
        token_sha256: &str,
        input_tokens: i64,
        output_tokens: i64,
        model: &str,
        caps: Caps,
    ) -> Result<Reservation, ReserveError> {
        let t = now();
        let period = period_of(t);
        let id = format!("res_{}", random_hex(12));
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| ReserveError::Store("lock".into()))?;
        let se = |e: rusqlite::Error| ReserveError::Store(e.to_string());
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(se)?;
        let found: Option<(String, i64, String, i64, i64, i64)> = tx
            .query_row(
                "SELECT n.id, t.generation, n.state, n.generation, n.input_limit, n.output_limit
                 FROM broker_tokens t JOIN nests n ON n.id = t.nest_id
                 WHERE t.token_sha256 = ?1",
                params![token_sha256],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(se)?;
        let Some((nest_id, token_gen, state, nest_gen, in_limit, out_limit)) = found else {
            return Err(ReserveError::Unauthorized);
        };
        let in_limit = caps.nest_input.map_or(in_limit, |c| in_limit.min(c));
        let out_limit = caps.nest_output.map_or(out_limit, |c| out_limit.min(c));
        if token_gen != nest_gen {
            return Err(ReserveError::Unauthorized);
        }
        if state != "running" {
            return Err(ReserveError::NotRunning);
        }
        let open: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM reservations WHERE nest_id = ?1 AND state = 'open'",
                params![nest_id],
                |r| r.get(0),
            )
            .map_err(se)?;
        if open > 0 {
            return Err(ReserveError::Busy);
        }
        tx.execute(
            "INSERT INTO usage (nest_id, period) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            params![nest_id, period],
        )
        .map_err(se)?;
        let (in_used, out_used, in_res, out_res): (i64, i64, i64, i64) = tx
            .query_row(
                "SELECT input_used, output_used, input_reserved, output_reserved FROM usage
                 WHERE nest_id = ?1 AND period = ?2",
                params![nest_id, period],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(se)?;
        if in_limit <= 0 || out_limit <= 0 {
            return Err(ReserveError::NoModel);
        }
        if in_used + in_res + input_tokens > in_limit {
            return Err(ReserveError::Exhausted("input"));
        }
        if out_used + out_res + output_tokens > out_limit {
            return Err(ReserveError::Exhausted("output"));
        }
        if caps.global_input.is_some() || caps.global_output.is_some() {
            let (all_in, all_out): (i64, i64) = tx
                .query_row(
                    "SELECT COALESCE(SUM(input_used + input_reserved + platform_input), 0),
                        COALESCE(SUM(output_used + output_reserved + platform_output), 0)
                     FROM usage WHERE period = ?1",
                    params![period],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(se)?;
            if caps.global_input.is_some_and(|c| all_in + input_tokens > c) {
                return Err(ReserveError::PlatformCap("input"));
            }
            if caps
                .global_output
                .is_some_and(|c| all_out + output_tokens > c)
            {
                return Err(ReserveError::PlatformCap("output"));
            }
        }
        tx.execute(
            "INSERT INTO reservations (id, nest_id, period, generation, input_reserved, output_reserved,
                model, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8)",
            params![id, nest_id, period, nest_gen, input_tokens, output_tokens, model, t],
        )
        .map_err(se)?;
        tx.execute(
            "UPDATE usage SET input_reserved = input_reserved + ?3, output_reserved = output_reserved + ?4
             WHERE nest_id = ?1 AND period = ?2",
            params![nest_id, period, input_tokens, output_tokens],
        )
        .map_err(se)?;
        tx.execute(
            "UPDATE nests SET last_active_at = ?2 WHERE id = ?1",
            params![nest_id, t],
        )
        .map_err(se)?;
        tx.commit().map_err(se)?;
        Ok(Reservation {
            id,
            nest_id,
            period,
            input_reserved: input_tokens,
            output_reserved: output_tokens,
        })
    }

    /// Settle a reservation exactly once: release it and charge according to the outcome.
    pub fn settle(
        &self,
        reservation_id: &str,
        outcome: Outcome,
        input_actual: i64,
        output_actual: i64,
        upstream_id: Option<&str>,
    ) -> Result<(), String> {
        self.with(|c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row: Option<(String, String, i64, i64)> = tx
                .query_row(
                    "SELECT nest_id, period, input_reserved, output_reserved FROM reservations
                     WHERE id = ?1 AND state = 'open'",
                    params![reservation_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((nest_id, period, in_res, out_res)) = row else {
                // Already settled (or never existed): settling twice must not charge twice.
                return Ok(());
            };
            let (charge_in, charge_out, plat_in, plat_out, label) = match outcome {
                Outcome::Ok => (input_actual.max(0), output_actual.max(0), 0, 0, "ok"),
                Outcome::NotSent => (0, 0, 0, 0, "not_sent"),
                Outcome::Unknown => (0, 0, in_res, out_res, "unknown"),
            };
            tx.execute(
                "UPDATE reservations SET state = 'settled', outcome = ?2, settled_at = ?3,
                    input_actual = ?4, output_actual = ?5, upstream_id = ?6 WHERE id = ?1",
                params![
                    reservation_id,
                    label,
                    now(),
                    charge_in,
                    charge_out,
                    upstream_id
                ],
            )?;
            tx.execute(
                "UPDATE usage SET input_reserved = input_reserved - ?3,
                    output_reserved = output_reserved - ?4,
                    input_used = input_used + ?5, output_used = output_used + ?6,
                    platform_input = platform_input + ?7, platform_output = platform_output + ?8,
                    requests = requests + ?9
                 WHERE nest_id = ?1 AND period = ?2",
                params![
                    nest_id,
                    period,
                    in_res,
                    out_res,
                    charge_in,
                    charge_out,
                    plat_in,
                    plat_out,
                    i64::from(outcome == Outcome::Ok)
                ],
            )?;
            tx.commit()
        })
    }

    /// Reservations left open by a crashed broker are settled as `Unknown` (platform cost).
    pub fn expire_reservations(&self, older_than_secs: i64) -> Result<usize, String> {
        let cutoff = now() - older_than_secs;
        let ids: Vec<String> = self.with(|c| {
            let mut stmt =
                c.prepare("SELECT id FROM reservations WHERE state = 'open' AND created_at < ?1")?;
            let rows = stmt.query_map(params![cutoff], |r| r.get(0))?;
            rows.collect()
        })?;
        for id in &ids {
            self.settle(id, Outcome::Unknown, 0, 0, None)?;
        }
        Ok(ids.len())
    }
}

#[cfg(test)]
impl Db {
    pub(crate) fn with_test_started_at(&self, id: &str, at: i64) {
        self.with(|c| {
            c.execute(
                "UPDATE nests SET started_at = ?2, last_active_at = ?2 WHERE id = ?1",
                params![id, at],
            )
        })
        .unwrap();
    }
}

const NEST_COLS: &str = "id, account_id, address, label, state, desired, stop_reason, generation,
    quota_gib, input_limit, output_limit, created_at, updated_at, started_at, last_active_at, last_error";

fn row_nest(r: &rusqlite::Row<'_>) -> rusqlite::Result<Nest> {
    Ok(Nest {
        id: r.get(0)?,
        account_id: r.get(1)?,
        address: r.get(2)?,
        label: r.get(3)?,
        state: r.get(4)?,
        desired: r.get(5)?,
        stop_reason: r.get(6)?,
        generation: r.get(7)?,
        quota_gib: r.get(8)?,
        input_limit: r.get(9)?,
        output_limit: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        started_at: r.get(13)?,
        last_active_at: r.get(14)?,
        last_error: r.get(15)?,
    })
}

/// One marketplace worker: which listing and buyer context it serves, and which release it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpWorker {
    pub nest_id: String,
    pub listing_id: String,
    pub buyer_context: String,
    pub release_id: String,
}

fn row_worker(r: &rusqlite::Row<'_>) -> rusqlite::Result<MpWorker> {
    Ok(MpWorker {
        nest_id: r.get(0)?,
        listing_id: r.get(1)?,
        buyer_context: r.get(2)?,
        release_id: r.get(3)?,
    })
}

const MP_WORKER_COLS: &str = "nest_id, listing_id, buyer_context, release_id";

impl Db {
    // ---- Marketplace pool (M1) -----------------------------------------------------------------

    /// `(image, release_digest, state)` of a release this host has heard of.
    pub fn mp_release(&self, release_id: &str) -> Result<Option<(String, String, String)>, String> {
        self.with(|c| {
            c.query_row(
                "SELECT image, release_digest, state FROM mp_releases WHERE release_id = ?1",
                params![release_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
    }

    /// Record an attested release. A release already known keeps its row: its image and digest
    /// never change, and one that is quarantined or revoked stays so.
    pub fn mp_admit(
        &self,
        release_id: &str,
        image: &str,
        release_digest: &str,
        statement_digest: &str,
    ) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO mp_releases
                    (release_id, image, release_digest, statement_digest, state, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'admitted', ?5) ON CONFLICT(release_id) DO NOTHING",
                params![release_id, image, release_digest, statement_digest, now()],
            )
            .map(|_| ())
        })
    }

    /// Quarantine or revoke. An unknown release is recorded in that state, so it can never be
    /// admitted later. Revocation is permanent: a trigger refuses the way back.
    pub fn mp_set_release_state(&self, release_id: &str, state: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO mp_releases
                    (release_id, image, release_digest, statement_digest, state, updated_at)
                 VALUES (?1, '', '', '', ?2, ?3)
                 ON CONFLICT(release_id) DO UPDATE SET state = excluded.state,
                    updated_at = excluded.updated_at",
                params![release_id, state, now()],
            )
            .map(|_| ())
        })
    }

    /// Drop a quarantine that was recorded before the release was ever admitted here (no image).
    /// Never touches an admitted or revoked release.
    pub fn mp_forget_unadmitted(&self, release_id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "DELETE FROM mp_releases WHERE release_id = ?1 AND image = '' AND state = 'quarantined'",
                params![release_id],
            )
            .map(|_| ())
        })
    }

    pub fn mp_worker_for(
        &self,
        listing_id: &str,
        context: &str,
    ) -> Result<Option<MpWorker>, String> {
        self.with(|c| {
            c.query_row(
                &format!(
                    "SELECT {MP_WORKER_COLS} FROM mp_workers
                      WHERE listing_id = ?1 AND buyer_context = ?2"
                ),
                params![listing_id, context],
                row_worker,
            )
            .optional()
        })
    }

    pub fn mp_worker(&self, nest_id: &str) -> Result<Option<MpWorker>, String> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {MP_WORKER_COLS} FROM mp_workers WHERE nest_id = ?1"),
                params![nest_id],
                row_worker,
            )
            .optional()
        })
    }

    /// Record a new worker with its ledger row, so the broker meters it like any Nest.
    pub fn mp_insert_worker(
        &self,
        w: &MpWorker,
        input_limit: i64,
        output_limit: i64,
    ) -> Result<(), String> {
        let t = now();
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO nests (id, account_id, address, label, state, desired, quota_gib,
                    input_limit, output_limit, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'marketplace', 'provisioning', 'running', 1, ?4, ?5, ?6, ?6)",
                params![
                    w.nest_id,
                    w.buyer_context,
                    w.listing_id,
                    input_limit,
                    output_limit,
                    t
                ],
            )?;
            tx.execute(
                "INSERT INTO mp_workers (nest_id, listing_id, buyer_context, release_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![w.nest_id, w.listing_id, w.buyer_context, w.release_id, t],
            )?;
            tx.commit()
        })
    }

    pub fn mp_set_worker_release(&self, nest_id: &str, release_id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE mp_workers SET release_id = ?2 WHERE nest_id = ?1",
                params![nest_id, release_id],
            )
            .map(|_| ())
        })
    }

    pub fn mp_workers_of_release(&self, release_id: &str) -> Result<Vec<MpWorker>, String> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {MP_WORKER_COLS} FROM mp_workers WHERE release_id = ?1 ORDER BY nest_id"
            ))?;
            let rows = stmt.query_map(params![release_id], row_worker)?;
            rows.collect()
        })
    }

    /// Ids of every marketplace worker: the first-party lifecycle leaves these alone.
    pub fn mp_worker_ids(&self) -> Result<std::collections::HashSet<String>, String> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT nest_id FROM mp_workers")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
    }

    /// `(nest_id, state, result)` of a job this host was given.
    #[allow(clippy::type_complexity)]
    pub fn mp_job(&self, job_id: &str) -> Result<Option<(String, String, Option<String>)>, String> {
        self.with(|c| {
            c.query_row(
                "SELECT nest_id, state, result FROM mp_jobs WHERE job_id = ?1",
                params![job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
    }

    pub fn mp_insert_job(&self, job_id: &str, nest_id: &str) -> Result<(), String> {
        let t = now();
        self.with(|c| {
            c.execute(
                "INSERT INTO mp_jobs (job_id, nest_id, state, created_at, updated_at)
                 VALUES (?1, ?2, 'running', ?3, ?3)",
                params![job_id, nest_id, t],
            )
            .map(|_| ())
        })
    }

    /// Finish a running job once; a finished job never changes again.
    pub fn mp_finish_job(
        &self,
        job_id: &str,
        state: &str,
        result: Option<&str>,
    ) -> Result<bool, String> {
        self.with(|c| {
            c.execute(
                "UPDATE mp_jobs SET state = ?2, result = ?3, updated_at = ?4
                  WHERE job_id = ?1 AND state = 'running'",
                params![job_id, state, result, now()],
            )
            .map(|n| n > 0)
        })
    }

    // ---- Hardening (W1) --------------------------------------------------------------------------

    pub fn mp_listing_stopped(&self, listing_id: &str) -> Result<bool, String> {
        self.with(|c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM mp_listing_stops WHERE listing_id = ?1)",
                params![listing_id],
                |r| r.get(0),
            )
        })
    }

    pub fn mp_set_listing_stopped(&self, listing_id: &str, stopped: bool) -> Result<(), String> {
        self.with(|c| {
            if stopped {
                c.execute(
                    "INSERT INTO mp_listing_stops (listing_id, stopped_at) VALUES (?1, ?2)
                     ON CONFLICT(listing_id) DO NOTHING",
                    params![listing_id, now()],
                )
            } else {
                c.execute(
                    "DELETE FROM mp_listing_stops WHERE listing_id = ?1",
                    params![listing_id],
                )
            }
            .map(|_| ())
        })
    }

    pub fn mp_workers_of_listing(&self, listing_id: &str) -> Result<Vec<MpWorker>, String> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {MP_WORKER_COLS} FROM mp_workers WHERE listing_id = ?1 ORDER BY nest_id"
            ))?;
            let rows = stmt.query_map(params![listing_id], row_worker)?;
            rows.collect()
        })
    }

    pub fn mp_set_worker_publisher(&self, nest_id: &str, publisher_id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO mp_worker_publishers (nest_id, publisher_id) VALUES (?1, ?2)
                 ON CONFLICT(nest_id) DO UPDATE SET publisher_id = excluded.publisher_id",
                params![nest_id, publisher_id],
            )
            .map(|_| ())
        })
    }

    /// Broker tokens a publisher's workers spent or hold reserved in `period`.
    pub fn mp_publisher_tokens(&self, publisher_id: &str, period: &str) -> Result<i64, String> {
        self.with(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(u.input_used + u.output_used + u.input_reserved
                        + u.output_reserved), 0)
                   FROM usage u JOIN mp_worker_publishers p ON p.nest_id = u.nest_id
                  WHERE p.publisher_id = ?1 AND u.period = ?2",
                params![publisher_id, period],
                |r| r.get(0),
            )
        })
    }

    /// A publisher's running workers, not counting `except`.
    pub fn mp_publisher_running(&self, publisher_id: &str, except: &str) -> Result<i64, String> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM mp_worker_publishers p JOIN nests n ON n.id = p.nest_id
                  WHERE p.publisher_id = ?1 AND n.state = 'running' AND n.id <> ?2",
                params![publisher_id, except],
                |r| r.get(0),
            )
        })
    }

    /// GiB promised to every live Nest volume on this host, workers included.
    pub fn allocated_gib(&self) -> Result<i64, String> {
        self.with(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(quota_gib), 0) FROM nests WHERE state <> 'deleted'",
                [],
                |r| r.get(0),
            )
        })
    }

    /// A destroyed worker: its jobs and worker rows go, its ledger row stays `deleted` (the
    /// tombstone tombstone replay and id allocation look for).
    pub fn mp_forget_worker(&self, nest_id: &str) -> Result<(), String> {
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM mp_jobs WHERE nest_id = ?1", params![nest_id])?;
            tx.execute(
                "DELETE FROM mp_workers WHERE nest_id = ?1",
                params![nest_id],
            )?;
            tx.execute(
                "DELETE FROM mp_worker_publishers WHERE nest_id = ?1",
                params![nest_id],
            )?;
            tx.execute(
                "DELETE FROM broker_tokens WHERE nest_id = ?1",
                params![nest_id],
            )?;
            tx.execute(
                "UPDATE nests SET state = 'deleted', desired = 'deleted', updated_at = ?2
                  WHERE id = ?1",
                params![nest_id, now()],
            )?;
            tx.commit()
        })
    }

    /// Every running job of a worker stops for security reasons.
    pub fn mp_stop_jobs_of(&self, nest_id: &str) -> Result<usize, String> {
        self.with(|c| {
            c.execute(
                "UPDATE mp_jobs SET state = 'stopped_security', updated_at = ?2
                  WHERE nest_id = ?1 AND state = 'running'",
                params![nest_id, now()],
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_nest(db: &Db, id: &str, input: i64, output: i64) -> String {
        db.insert_nest(id, "acc", "/k/x", "t", 1, input, output)
            .unwrap();
        let g = db.next_generation(id).unwrap();
        db.set_state(id, "running", "running", None, None).unwrap();
        let token = format!("tok-{id}");
        db.set_broker_token(id, &crate::util::sha256_hex(token.as_bytes()), g)
            .unwrap();
        crate::util::sha256_hex(token.as_bytes())
    }

    #[test]
    fn reservations_are_atomic_and_settle_once() {
        let db = Db::open_memory().unwrap();
        let tok = running_nest(&db, "nst_0000000000000001", 1000, 100);
        let r = db.reserve(&tok, 600, 80, "m").unwrap();
        // One in flight at a time.
        assert_eq!(db.reserve(&tok, 1, 1, "m"), Err(ReserveError::Busy));
        db.settle(&r.id, Outcome::Ok, 500, 60, Some("gen-1"))
            .unwrap();
        // Settling twice charges once.
        db.settle(&r.id, Outcome::Ok, 500, 60, Some("gen-1"))
            .unwrap();
        let u = db.usage("nst_0000000000000001", &r.period).unwrap();
        assert_eq!((u.input_used, u.output_used), (500, 60));
        assert_eq!((u.input_reserved, u.output_reserved), (0, 0));
        assert_eq!(u.requests, 1);
        // 500 + 600 > 1000: refused before any call is made.
        assert_eq!(
            db.reserve(&tok, 600, 10, "m"),
            Err(ReserveError::Exhausted("input"))
        );
        assert_eq!(
            db.reserve(&tok, 10, 50, "m"),
            Err(ReserveError::Exhausted("output"))
        );
    }

    #[test]
    fn a_nest_without_a_model_allowance_is_told_so_not_exhausted() {
        let db = Db::open_memory().unwrap();
        let tok = running_nest(&db, "nst_0000000000000009", 0, 0);
        assert_eq!(db.reserve(&tok, 1, 1, "m"), Err(ReserveError::NoModel));
    }

    #[test]
    fn operator_caps_bound_each_nest_and_all_nests() {
        let db = Db::open_memory().unwrap();
        let tok_a = running_nest(&db, "nst_00000000000000a1", 10_000, 1_000);
        let tok_b = running_nest(&db, "nst_00000000000000b2", 10_000, 1_000);
        let caps = Caps {
            nest_input: Some(500),
            nest_output: Some(100),
            global_input: Some(800),
            global_output: Some(1_000),
        };
        // The per-Nest cap (500) wins over the Nest's own allowance (10,000).
        assert_eq!(
            db.reserve_capped(&tok_a, 600, 10, "m", caps),
            Err(ReserveError::Exhausted("input"))
        );
        let r = db.reserve_capped(&tok_a, 400, 10, "m", caps).unwrap();
        db.settle(&r.id, Outcome::Ok, 400, 10, None).unwrap();
        // 400 used by A + 450 for B > 800 for all Nests together.
        assert_eq!(
            db.reserve_capped(&tok_b, 450, 10, "m", caps),
            Err(ReserveError::PlatformCap("input"))
        );
        assert!(db.reserve_capped(&tok_b, 400, 10, "m", caps).is_ok());
        // Without caps only the Nest's own allowance applies.
        assert!(db.reserve(&tok_a, 50, 10, "m").is_ok());
    }

    #[test]
    fn unknown_outcomes_go_to_the_platform_budget() {
        let db = Db::open_memory().unwrap();
        let tok = running_nest(&db, "nst_0000000000000002", 1000, 100);
        let r = db.reserve(&tok, 300, 50, "m").unwrap();
        db.settle(&r.id, Outcome::Unknown, 0, 0, None).unwrap();
        let u = db.usage("nst_0000000000000002", &r.period).unwrap();
        assert_eq!((u.input_used, u.output_used), (0, 0));
        assert_eq!((u.platform_input, u.platform_output), (300, 50));
    }

    #[test]
    fn stale_tokens_and_stopped_nests_are_refused() {
        let db = Db::open_memory().unwrap();
        let id = "nst_0000000000000003";
        let tok = running_nest(&db, id, 1000, 100);
        assert_eq!(
            db.reserve("nope", 1, 1, "m"),
            Err(ReserveError::Unauthorized)
        );
        db.set_state(id, "paused", "paused", None, None).unwrap();
        assert_eq!(db.reserve(&tok, 1, 1, "m"), Err(ReserveError::NotRunning));
        // A restart is a new generation: the old token is fenced even before it is replaced.
        db.set_state(id, "running", "running", None, None).unwrap();
        db.next_generation(id).unwrap();
        assert_eq!(db.reserve(&tok, 1, 1, "m"), Err(ReserveError::Unauthorized));
    }

    #[test]
    fn leases_exclude_other_holders() {
        let db = Db::open_memory().unwrap();
        let id = "nst_0000000000000004";
        db.insert_nest(id, "acc", "/k/x", "t", 1, 1, 1).unwrap();
        assert!(db.acquire_lease(id, "a").unwrap());
        assert!(db.acquire_lease(id, "a").unwrap());
        assert!(!db.acquire_lease(id, "b").unwrap());
        db.release_lease(id);
        assert!(db.acquire_lease(id, "b").unwrap());
    }

    #[test]
    fn running_time_counts_runs() {
        let db = Db::open_memory().unwrap();
        let id = "nst_0000000000000005";
        db.insert_nest(id, "acc", "/k/x", "t", 1, 1, 1).unwrap();
        db.open_run(id, 1, 10).unwrap();
        assert!(db.running_secs(id, 0).unwrap() >= 0);
        db.close_runs(id, "test").unwrap();
    }
}
