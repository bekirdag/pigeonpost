//! Unannounced checks of publisher-hosted services (developer program plan §4, conformance
//! detectability). The announced conformance run (`marketplace_conformance`) tells the service it
//! is being reviewed; a service could behave only then. Two checks it cannot tell apart from real
//! use:
//!
//! - **Shadow jobs.** An ordinary hosted job (`nest_hosted_jobs`): a real `listing_id`, a normal
//!   job id, the plain job message with no `review` key, and input drawn from a pool of realistic,
//!   varied values. It is run by the platform owner's account and charged to nobody (no credit
//!   hold is placed, so nothing is captured and the publisher is not paid). Staff start one with
//!   `POST /v1/staff/marketplace/listings/{listing}/shadow-jobs`; with
//!   `POSTBOX_MARKETPLACE_SHADOW_DAYS` (30 by default, `0` off) every published publisher-hosted
//!   listing also gets one at a random time, about once per that many days. Its result gets the
//!   conformance checks (status, PDF structure) and ClamAV and YARA on the build plane; a failure
//!   flags the release for staff (`release_security_flags`, source `shadow_job`), and a malware
//!   hit also stops the listing.
//! - **Sampled buyer results.** `POSTBOX_MARKETPLACE_RESULT_SAMPLE_PERCENT` (5 by default, `0`
//!   off) of real buyers' succeeded results are sent to the build plane's ClamAV and YARA after
//!   they are recorded (asynchronously: the buyer's delivery does not wait). A hit stops the
//!   listing (the kill switch, actor `system:analysis`) and flags it for staff (source
//!   `result_sample`, action `quarantined`); staff restore it with the existing kill route.
//!
//! Only `nest_shadow_jobs` and `nest_result_scans` say a job is a check; neither is visible to
//! the publisher. A shadow job's result is never delivered anywhere.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `POST /v1/staff/marketplace/listings/{listing}/shadow-jobs` | `NestReview` | staff with step-up and no stake |

use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, not_found};
use crate::marketplace_hardening::Buildd;
use crate::marketplace_hosted::{platform_mailbox, publishable_in};
use crate::marketplace_m1::conflict_code;
use crate::marketplace_m2::{new_id, release_in, staff_gate};
use crate::policy::Action;
use crate::{now_unix, AppState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/staff/marketplace/listings/{listing}/shadow-jobs",
        post(staff_shadow_job),
    )
}

/// Realistic, varied input for shadow jobs. Each field gets one at random (cut to its limit), and
/// an optional field is sometimes left out, as buyers do.
const INPUT_POOL: &[&str] = &[
    "Senior backend engineer, Istanbul office, hybrid",
    "Product designer with fintech experience",
    "Head of people operations for a 40-person startup",
    "Data analyst, SQL and Python, remote within Turkey",
    "Customer support lead, Turkish and English",
    "Please keep the report to two pages.",
    "Focus on the last three years.",
    "Compare with the market median in Ankara.",
    "Marketing manager for a B2B software company",
    "DevOps engineer, Kubernetes, on-call rotation",
    "Office manager, part time, Izmir",
    "Mobile developer for iOS and Android",
    "Finance controller, IFRS, manufacturing",
    "Include salary bands if you have them.",
];

/// A uniform pick in `0..n`.
fn pick(n: usize) -> usize {
    let hex = crate::rand_hex(4);
    (u32::from_str_radix(&hex, 16).unwrap_or(0) as usize) % n.max(1)
}

/// Input for one operation's fields, from [`INPUT_POOL`].
pub(crate) fn shadow_input(op: &Value) -> Value {
    let mut input = serde_json::Map::new();
    for f in op["fields"].as_array().into_iter().flatten() {
        let Some(name) = f["name"].as_str() else {
            continue;
        };
        if f["required"] != true && pick(10) < 3 {
            continue;
        }
        let max = f["max_chars"].as_u64().unwrap_or(1).max(1) as usize;
        let text: String = INPUT_POOL[pick(INPUT_POOL.len())]
            .chars()
            .take(max)
            .collect();
        input.insert(name.to_string(), json!(text));
    }
    Value::Object(input)
}

/// Queue one shadow job on a published publisher-hosted listing: a real hosted job of the
/// platform owner's account, with no charge. Returns the job id.
pub(crate) fn create_in(
    c: &Connection,
    listing: &str,
    owner: &str,
    deliver_to: &str,
    origin: &str,
    who: Option<&str>,
    now: i64,
) -> GResult<String> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT release_id, status, hosting FROM nest_listings WHERE id = ?1",
            [listing],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((release, status, hosting)) = row else {
        return Err(not_found());
    };
    if hosting != "publisher" || status != "published" {
        return Err(conflict_code(
            "not_publisher_hosted",
            "shadow jobs are for published publisher-hosted listings",
        ));
    }
    let open: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM nest_shadow_jobs s JOIN nest_hosted_jobs j ON j.id = s.job_id
          WHERE s.listing_id = ?1 AND j.state IN ('queued','running'))",
        [listing],
        |r| r.get(0),
    )?;
    if open {
        return Err(conflict_code(
            "shadow_job_open",
            "this listing already has a shadow job running",
        ));
    }
    let rel = release_in(c, &release)?.ok_or_else(not_found)?;
    publishable_in(c, &rel)?;
    let ops: Vec<Value> = rel.manifest["operations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if ops.is_empty() {
        return Err(conflict_code(
            "no_operation",
            "this listing has no operation",
        ));
    }
    let op = &ops[pick(ops.len())];
    let hours = op["deadline_hours"]
        .as_i64()
        .unwrap_or(72)
        .clamp(1, 14 * 24);
    let job = new_id("job");
    c.execute(
        "INSERT INTO nest_hosted_jobs (id, account_id, listing_id, release_id, publisher_id,
            operation, operation_version, input, deliver_to, service_mailbox, state,
            next_attempt_at, deadline_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', ?11, ?12, ?11, ?11)",
        params![
            job,
            owner,
            listing,
            rel.id,
            rel.publisher_id,
            op["name"].as_str().unwrap_or(""),
            op["version"].as_str().unwrap_or(""),
            shadow_input(op).to_string(),
            deliver_to,
            rel.manifest["service_mailbox"].as_str().unwrap_or(""),
            now,
            now + hours * 3600
        ],
    )?;
    c.execute(
        "INSERT INTO nest_shadow_jobs (job_id, listing_id, release_id, origin, requested_by,
            created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![job, listing, rel.id, origin, who, now],
    )?;
    Ok(job)
}

/// The account and mailbox shadow jobs run from: the platform owner's marketplace mailbox.
async fn runner(state: &AppState) -> GResult<(String, String)> {
    let owner = state
        .platform_owner
        .as_deref()
        .map(str::to_string)
        .ok_or_else(|| {
            GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "marketplace_mailbox_unavailable",
                "no platform owner on this postbox",
            )
        })?;
    let mailbox = platform_mailbox(state).await?;
    Ok((owner, mailbox.address))
}

/// `POST /v1/staff/marketplace/listings/{listing}/shadow-jobs` — run one unannounced check now.
async fn staff_shadow_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing): Path<String>,
) -> Response {
    let result = async {
        let l = listing.clone();
        let publisher: Option<String> = state
            .store
            .group_read(move |c| -> GResult<Option<String>> {
                Ok(c.query_row(
                    "SELECT publisher_id FROM nest_listings WHERE id = ?1",
                    [&l],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await?;
        let publisher = publisher.ok_or_else(not_found)?;
        let (_, who) = staff_gate(
            &state,
            &headers,
            Action::NestReview,
            "shadow-jobs",
            Some(publisher),
        )
        .await?;
        let (owner, deliver) = runner(&state).await?;
        let now = now_unix() as i64;
        let job = state
            .store
            .group_tx(move |tx| create_in(tx, &listing, &owner, &deliver, "staff", Some(&who), now))
            .await?;
        let s = state.clone();
        tokio::spawn(async move {
            crate::marketplace_hosted::dispatch_once(&s).await;
        });
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": { "job_id": job, "shadow": true } })),
        ))
    }
    .await;
    into_response(result)
}

fn sample_percent() -> u32 {
    // Off in tests unless a test asks (`check_result_sampled`): sampling is random.
    let default = if cfg!(test) { 0 } else { 5 };
    std::env::var("POSTBOX_MARKETPLACE_RESULT_SAMPLE_PERCENT")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|p| *p <= 100)
        .unwrap_or(default)
}

fn shadow_days() -> i64 {
    std::env::var("POSTBOX_MARKETPLACE_SHADOW_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| (0..=365).contains(d))
        .unwrap_or(30)
}

/// What the result route calls once a hosted job's result is recorded. A shadow job's result is
/// checked (and never delivered); a sample of real succeeded results goes to the malware scan.
/// Asynchronous: the result route and the buyer's delivery never wait for it.
pub(crate) fn after_result(
    state: &AppState,
    job: &str,
    status: &str,
    files: Vec<(String, Vec<u8>)>,
) {
    let (s, job, status) = (state.clone(), job.to_string(), status.to_string());
    tokio::spawn(async move {
        let _ = check_result(&s, &job, &status, files).await;
    });
}

pub(crate) async fn check_result(
    state: &AppState,
    job: &str,
    status: &str,
    files: Vec<(String, Vec<u8>)>,
) -> GResult<()> {
    check_result_sampled(state, job, status, files, sample_percent()).await
}

/// [`check_result`] with the sampling rate given.
pub(crate) async fn check_result_sampled(
    state: &AppState,
    job: &str,
    status: &str,
    files: Vec<(String, Vec<u8>)>,
    percent: u32,
) -> GResult<()> {
    let j = job.to_string();
    type Row = (String, String, bool);
    let row: Option<Row> = state
        .store
        .group_read(move |c| -> GResult<Option<Row>> {
            Ok(c.query_row(
                "SELECT j.listing_id, j.release_id,
                        EXISTS (SELECT 1 FROM nest_shadow_jobs s WHERE s.job_id = j.id)
                   FROM nest_hosted_jobs j WHERE j.id = ?1",
                [&j],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
        })
        .await?;
    let Some((listing, release, shadow)) = row else {
        return Ok(());
    };
    let now = now_unix() as i64;
    if shadow {
        // A shadow result is the platform's own: it goes nowhere.
        let j = job.to_string();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nest_hosted_jobs SET delivery = 'delivered', updated_at = ?2
                      WHERE id = ?1",
                    params![j, now],
                )?;
                Ok(())
            })
            .await?;
        let mut problems = Vec::new();
        if status != "succeeded" {
            problems.push(problem(
                "shadow.failed",
                job,
                "the service answered the job as failed",
            ));
        }
        for (i, (_, bytes)) in files.iter().enumerate() {
            for p in crate::marketplace_conformance::pdf_problems(bytes) {
                problems.push(problem(
                    p,
                    job,
                    format!("files[{i}] is not a PDF a buyer can open"),
                ));
            }
        }
        if !problems.is_empty() {
            return decide(state, job, &listing, &release, problems, false, now).await;
        }
    } else if status != "succeeded" || files.is_empty() || pick(100) >= percent as usize {
        return Ok(());
    }
    if matches!(state.nests.marketplace.buildd, Buildd::Disabled) {
        return Ok(());
    }
    let scan = new_id("scn");
    let body = json!({
        "scan_id": scan, "kind": "files",
        "files": files.iter().map(|(name, bytes)| json!({
            "name": name, "media_type": "application/pdf",
            "content_b64": crate::groups::b64(bytes),
        })).collect::<Vec<_>>(),
    });
    let accepted = matches!(
        state
            .nests
            .marketplace
            .buildd_call(reqwest::Method::POST, "/v1/scans", Some(body))
            .await,
        Ok((200..=299, _))
    );
    let (j, kind) = (job.to_string(), if shadow { "shadow" } else { "sample" });
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "INSERT INTO nest_result_scans (scan_id, job_id, listing_id, release_id, kind,
                    state, submitted_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CASE WHEN ?6 = 'failed' THEN ?7 END)",
                params![
                    scan,
                    j,
                    listing,
                    release,
                    kind,
                    if accepted { "submitted" } else { "failed" },
                    now
                ],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn problem(rule: &str, job: &str, message: impl Into<String>) -> Value {
    json!({
        "id": format!("shadow:{rule}:{job}"), "severity": "high", "tool": "conformance",
        "rule": rule, "message": message.into(), "blocking": true,
    })
}

/// Stop a listing for staff: the kill switch, as staff would set it, by the analysis plane.
pub(crate) fn quarantine_in(
    c: &Connection,
    listing: &str,
    reason: &str,
    platform_owner: Option<&str>,
    now: i64,
) -> GResult<()> {
    c.execute(
        "INSERT INTO marketplace_kills (id, target_kind, target_id, action, reason, actor, created_at)
         VALUES (?1, 'listing', ?2, 'kill', ?3, 'system:analysis', ?4)",
        params![format!("kil_{}", crate::groups::rand_b32(16)), listing, reason, now],
    )?;
    c.execute(
        "INSERT INTO marketplace_kill_state (target_kind, target_id, killed, updated_at)
         VALUES ('listing', ?1, 1, ?2)
         ON CONFLICT(target_kind, target_id) DO UPDATE SET killed = 1, updated_at = excluded.updated_at",
        params![listing, now],
    )?;
    crate::marketplace_hardening::security_event_in(
        c,
        "quarantined",
        None,
        Some(listing),
        platform_owner,
        now,
    )
}

/// A check's verdict: flag the release, stop the listing on a malware hit, and record a shadow
/// job's verdict.
async fn decide(
    state: &AppState,
    job: &str,
    listing: &str,
    release: &str,
    findings: Vec<Value>,
    malware: bool,
    now: i64,
) -> GResult<()> {
    let (j, l, r) = (job.to_string(), listing.to_string(), release.to_string());
    let owner = state.platform_owner.as_deref().map(str::to_string);
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let shadow: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM nest_shadow_jobs WHERE job_id = ?1)",
                [&j],
                |row| row.get(0),
            )?;
            if shadow {
                tx.execute(
                    "UPDATE nest_shadow_jobs SET verdict = ?2, findings = ?3, decided_at = ?4
                      WHERE job_id = ?1 AND verdict IS NULL",
                    params![
                        j,
                        if findings.is_empty() {
                            "passed"
                        } else {
                            "failed"
                        },
                        Value::Array(findings.clone()).to_string(),
                        now
                    ],
                )?;
            }
            if findings.is_empty() {
                return Ok(());
            }
            let action = if malware { "quarantined" } else { "none" };
            if malware {
                quarantine_in(
                    tx,
                    &l,
                    "a hosted result matched a malware signature",
                    owner.as_deref(),
                    now,
                )?;
            }
            crate::marketplace_rescan::flag_in(
                tx,
                &r,
                Some(&l),
                if shadow {
                    "shadow_job"
                } else {
                    "result_sample"
                },
                &j,
                &findings,
                action,
                now,
            )?;
            Ok(())
        })
        .await?;
    crate::marketplace_hardening::wake_new_events(state).await;
    Ok(())
}

/// Read finished result scans; a shadow job that ended without a result fails; schedule random
/// shadow jobs. Called from the hosted-job worker.
pub(crate) async fn work_once(state: &AppState) -> usize {
    let now = now_unix() as i64;
    let mut n = 0;
    // Result scans.
    let open: Vec<(String, String, String, String)> = state
        .store
        .group_tx(move |c| -> GResult<Vec<(String, String, String, String)>> {
            c.execute(
                "UPDATE nest_result_scans SET state = 'failed', finished_at = ?1
                  WHERE state = 'submitted' AND submitted_at < ?2",
                params![now, now - crate::marketplace_rescan::STALE_SECS],
            )?;
            let mut stmt = c.prepare(
                "SELECT scan_id, job_id, listing_id, release_id FROM nest_result_scans
                  WHERE state = 'submitted' ORDER BY submitted_at LIMIT 20",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for (scan, job, listing, release) in open {
        let Ok((code, v)) = state
            .nests
            .marketplace
            .buildd_call(reqwest::Method::GET, &format!("/v1/scans/{scan}"), None)
            .await
        else {
            continue;
        };
        let v = if v["data"].is_object() {
            v["data"].clone()
        } else {
            v
        };
        let hits: Option<Vec<Value>> = match (code, v["state"].as_str()) {
            (200, Some("succeeded")) => Some(
                v["result"]["findings"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| {
                        f["blocking"] == true
                            && f["rule"]
                                .as_str()
                                .is_some_and(|r| r.starts_with("malware."))
                    })
                    .collect(),
            ),
            (404, _) | (200, Some("failed")) => None,
            _ => continue,
        };
        let s = scan.clone();
        let done = hits.is_some();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nest_result_scans SET state = ?2, finished_at = ?3 WHERE scan_id = ?1",
                    params![s, if done { "done" } else { "failed" }, now],
                )?;
                Ok(())
            })
            .await;
        if let Some(hits) = hits {
            let malware = !hits.is_empty();
            let _ = decide(state, &job, &listing, &release, hits, malware, now).await;
        }
        n += 1;
    }
    // Shadow jobs that ended without a result (expired, or the service mailbox lost).
    let ended: Vec<(String, String, String, String)> = state
        .store
        .group_read(|c| -> GResult<Vec<(String, String, String, String)>> {
            let mut stmt = c.prepare(
                "SELECT s.job_id, s.listing_id, s.release_id, j.state FROM nest_shadow_jobs s
                   JOIN nest_hosted_jobs j ON j.id = s.job_id
                  WHERE s.verdict IS NULL AND j.result_digest IS NULL
                    AND j.state IN ('failed','expired') LIMIT 20",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for (job, listing, release, st) in ended {
        let f = vec![problem(
            "shadow.no_result",
            &job,
            format!("the job ended {st} without a result"),
        )];
        let _ = decide(state, &job, &listing, &release, f, false, now).await;
        n += 1;
    }
    // A shadow job per published hosted listing, at random, about once per `shadow_days` each
    // (this runs every 20 seconds). At most one is started per pass.
    let days = shadow_days();
    if days > 0 {
        let candidates: Vec<String> = state
            .store
            .group_read(|c| -> GResult<Vec<String>> {
                let mut stmt = c.prepare(
                    "SELECT id FROM nest_listings WHERE hosting = 'publisher'
                       AND status = 'published'",
                )?;
                let rows = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .unwrap_or_default();
        let passes = (days as usize) * 86_400 / 20;
        if let Some(l) = candidates.into_iter().find(|_| pick(passes) == 0) {
            if let Ok((owner, deliver)) = runner(state).await {
                if state
                    .store
                    .group_tx(move |tx| create_in(tx, &l, &owner, &deliver, "schedule", None, now))
                    .await
                    .is_ok()
                {
                    n += 1;
                }
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_input_is_varied_realistic_and_within_limits() {
        let op = json!({ "fields": [
            { "name": "role", "max_chars": 20, "required": true },
            { "name": "notes", "max_chars": 2000, "required": false },
        ]});
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            let v = shadow_input(&op);
            let role = v["role"].as_str().unwrap();
            assert!(role.chars().count() <= 20);
            assert!(!role.contains("review") && !role.contains("Pigeonpost"));
            seen.insert(role.to_string());
        }
        assert!(seen.len() > 5, "{seen:?}");
    }
}
