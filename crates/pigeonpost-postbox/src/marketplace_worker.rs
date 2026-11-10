//! What a running marketplace job reports through nestd's broker (developer-kit routes
//! `POST /v1/progress` and `POST /v1/effects`, nestd `worker_api`), as the postbox records it.
//!
//! The dispatcher's poll (`marketplace_m1::poll_once`) hands each answer here:
//!
//! - **Progress**: a new update becomes `nest.job.progress` for the submitting account, with the
//!   fraction (the message is the publisher's text and stays out of events).
//! - **Effects**: the job's effect records (key, kind, state; never their bodies) are kept with
//!   the job. An effect whose outcome is unknown counts in `effects_outcome_unknown` on
//!   `GET /v1/nest-jobs/{job}` and sends `nest.job.reconciling` once: it is reconciled, never
//!   retried blindly.

use crate::groups::GResult;
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS marketplace_job_activity (
    job_id           TEXT PRIMARY KEY,
    progress_updates INTEGER NOT NULL DEFAULT 0,
    fraction         REAL,
    effects          TEXT NOT NULL DEFAULT '[]',
    outcome_unknown  INTEGER NOT NULL DEFAULT 0,
    updated_at       INTEGER NOT NULL
);
";

/// Record what nestd reported for `job` (`progress`, `effects` of its poll answer).
pub(crate) async fn record(state: &AppState, job: &str, polled: &Value) {
    let progress = polled["progress"].clone();
    let effects: Vec<Value> = polled["effects"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(16)
                .map(|e| {
                    json!({
                        "key": e["key"], "kind": e["kind"], "state": e["state"],
                        "reference": e["reference"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if progress.is_null() && effects.is_empty() {
        return;
    }
    let (jid, now) = (job.to_string(), crate::now_unix() as i64);
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let before: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT progress_updates, outcome_unknown FROM marketplace_job_activity
                      WHERE job_id = ?1",
                    [&jid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (seen, unknown_before) = before.unwrap_or((0, 0));
            let updates = progress["updates"].as_i64().unwrap_or(seen);
            let unknown = effects
                .iter()
                .filter(|e| e["state"] == "outcome_unknown")
                .count() as i64;
            tx.execute(
                "INSERT INTO marketplace_job_activity (job_id, progress_updates, fraction, effects,
                    outcome_unknown, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(job_id) DO UPDATE SET progress_updates = ?2,
                    fraction = COALESCE(?3, fraction), effects = ?4, outcome_unknown = ?5,
                    updated_at = ?6",
                params![
                    jid,
                    updates.max(seen),
                    progress["fraction"].as_f64(),
                    Value::Array(effects).to_string(),
                    unknown,
                    now
                ],
            )?;
            if updates > seen {
                crate::marketplace_hardening::job_event_in(
                    tx,
                    &jid,
                    "nest.job.progress",
                    &json!({ "execution": "running", "progress": progress["fraction"] }),
                    now,
                )?;
            }
            if unknown > unknown_before {
                crate::marketplace_hardening::job_event_in(
                    tx,
                    &jid,
                    "nest.job.reconciling",
                    &json!({ "execution": "reconciling" }),
                    now,
                )?;
            }
            Ok(())
        })
        .await;
}

/// For `GET /v1/nest-jobs/{job}`: effects whose outcome is unknown, and the latest progress.
pub(crate) fn activity_in(c: &Connection, job: &str) -> rusqlite::Result<(i64, Option<f64>)> {
    Ok(c.query_row(
        "SELECT outcome_unknown, fraction FROM marketplace_job_activity WHERE job_id = ?1",
        [job],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .unwrap_or((0, None)))
}
