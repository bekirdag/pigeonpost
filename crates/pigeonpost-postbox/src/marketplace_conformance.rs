//! The hosted conformance run (developer program plan §4, "Publisher-hosted integrations"): the
//! automated stage an integration passes before a staff member may approve it.
//!
//! For every open integration review without one, one **review job** per operation is sent to the
//! service mailbox through the hosted-job path: the same message (with `review: true` and no
//! listing, so the publisher can tell), the same sender (the platform marketplace mailbox), the
//! same result route (`POST /v1/nest-jobs/{job}/result`, the service mailbox's own credential) and
//! the same result rules. A review job is never charged and never delivered to a buyer.
//!
//! Each job must:
//! - be answered `succeeded` with a result the contract accepts (status, summary, PDF files);
//! - carry structurally valid PDFs (header, objects, `startxref`, `/Root`, `%%EOF`, not encrypted);
//! - pass ClamAV and YARA on every file, run by the build plane in gVisor with no network
//!   (`POST /v1/scans` on `pigeonpost-nest-buildd`); any hit fails;
//! - arrive before the operation's deadline;
//! - come from a service mailbox that is still the publisher's, at dispatch and at the result.
//!
//! When every job has ended, the stage is recorded as `behavioral_test` (the stage name the review
//! tables allow; its evidence says `kind: hosted_conformance`): `passed`, or `failed`, which blocks
//! the review like any failed stage. A result the contract refuses is answered as for a buyer's
//! job (the service may retry until the deadline); the last refusal is the job's finding if it
//! never sends a good one.
//!
//! A job lost on Pigeonpost's side is never the publisher's failure: one never sent, one whose
//! scan the build plane lost (a restart) or could not run, or one whose result could not be taken
//! for scanning (`503 scan_unavailable`) by its deadline becomes `platform_error` and is replaced
//! by a fresh job (new id, full deadline), up to three times per operation; after that the stage
//! waits for staff. The newest job of each operation decides the stage.
//!
//! The announced run can be gamed by a service that behaves only when told it is reviewed; the
//! unannounced checks in `marketplace_shadow` cover that.

use crate::groups::{GErr, GResult};
use crate::marketplace::{deny, facts, iso, not_found, parse};
use crate::marketplace_hardening::Buildd;
use crate::marketplace_hosted::{
    backoff, checked_result, msg_ref_for, platform_mailbox, review_job_message, service_owned_in,
    ResultReq,
};
use crate::marketplace_m1::{canonical_json, conflict_code, digest_of};
use crate::marketplace_m2::{new_id, release_in};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};

/// Who records the conformance stage.
const CONFORMANCE: &str = "system:conformance";
/// What each field of a review job's input says.
const REVIEW_INPUT: &str = "Pigeonpost review job: synthetic input, no buyer data";
/// Most operations one conformance run sends a job for (the descriptor allows 20).
const MAX_JOBS: usize = 20;

/// One finding of the conformance run, in the analysis plane's shape.
fn finding(rule: &str, job: &str, message: impl Into<String>) -> Value {
    json!({
        "id": format!("conformance:{rule}:{job}"),
        "severity": "high",
        "tool": "conformance",
        "rule": rule,
        "message": message.into(),
        "blocking": true,
    })
}

/// What a buyer could not open as a PDF, beyond the result route's own check (header, `%%EOF`).
pub(crate) fn pdf_problems(bytes: &[u8]) -> Vec<&'static str> {
    let has = |needle: &[u8], within: &[u8]| within.windows(needle.len()).any(|w| w == needle);
    let tail = &bytes[bytes.len().saturating_sub(2048)..];
    let mut out = Vec::new();
    if !(bytes.starts_with(b"%PDF-1.") || bytes.starts_with(b"%PDF-2.")) {
        out.push("pdf.header");
    }
    if !has(b"%%EOF", &bytes[bytes.len().saturating_sub(1024)..]) {
        out.push("pdf.eof");
    }
    if !has(b"startxref", tail) {
        out.push("pdf.xref");
    }
    if !has(b" obj", bytes) || !has(b"endobj", bytes) {
        out.push("pdf.objects");
    }
    if !has(b"/Root", bytes) {
        out.push("pdf.root");
    }
    if has(b"/Encrypt", bytes) {
        out.push("pdf.encrypted");
    }
    out
}

/// The input of a review job: every field of the operation, filled within its limit.
fn review_input(op: &Value) -> Value {
    let mut input = serde_json::Map::new();
    for f in op["fields"].as_array().into_iter().flatten() {
        let max = f["max_chars"].as_u64().unwrap_or(1).max(1) as usize;
        if let Some(name) = f["name"].as_str() {
            input.insert(
                name.to_string(),
                json!(REVIEW_INPUT.chars().take(max).collect::<String>()),
            );
        }
    }
    Value::Object(input)
}

/// Queue the review jobs of every open integration review that has none and no recorded stage.
pub(crate) fn start_in(tx: &Transaction<'_>, now: i64) -> GResult<usize> {
    let cases: Vec<(String, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT c.id, c.release_id, c.publisher_id FROM review_cases c
              WHERE c.kind = 'integration' AND c.status = 'open'
                AND NOT EXISTS (SELECT 1 FROM review_decisions d
                                 WHERE d.case_id = c.id AND d.stage = 'behavioral_test')
                AND NOT EXISTS (SELECT 1 FROM nest_review_jobs j WHERE j.case_id = c.id)
              ORDER BY c.created_at LIMIT 20",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut n = 0;
    for (case, release, publisher) in cases {
        let Some(rel) = release_in(tx, &release)? else {
            continue;
        };
        let mailbox = rel.manifest["service_mailbox"].as_str().unwrap_or("");
        for op in rel.manifest["operations"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_JOBS)
        {
            let hours = op["deadline_hours"]
                .as_i64()
                .unwrap_or(72)
                .clamp(1, 14 * 24);
            tx.execute(
                "INSERT INTO nest_review_jobs (id, case_id, release_id, publisher_id, operation,
                    operation_version, input, service_mailbox, state, next_attempt_at,
                    deadline_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9, ?10, ?9, ?9)",
                params![
                    new_id("job"),
                    case,
                    release,
                    publisher,
                    op["name"].as_str().unwrap_or(""),
                    op["version"].as_str().unwrap_or(""),
                    review_input(op).to_string(),
                    mailbox,
                    now,
                    now + hours * 3600
                ],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

/// End a review job with its findings (appended to any it has).
fn fail_in(c: &Connection, job: &str, found: Vec<Value>, now: i64) -> rusqlite::Result<usize> {
    let had: String = c
        .query_row(
            "SELECT problems FROM nest_review_jobs WHERE id = ?1",
            [job],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "[]".into());
    let mut all: Vec<Value> = serde_json::from_str(&had).unwrap_or_default();
    all.extend(found);
    c.execute(
        "UPDATE nest_review_jobs SET state = 'failed', problems = ?2, finished_at = ?3,
            updated_at = ?3 WHERE id = ?1 AND state IN ('queued','running','scanning')",
        params![job, Value::Array(all).to_string(), now],
    )
}

/// Send queued review jobs; a mailbox that is no longer the publisher's fails its job.
async fn dispatch_once(state: &AppState, now: i64) -> usize {
    type Due = (String, String, String, String, String, i64, i64);
    let due: Vec<Due> = state
        .store
        .group_read(move |c| -> GResult<Vec<Due>> {
            let mut stmt = c.prepare(
                "SELECT id, publisher_id, operation, input, service_mailbox, deadline_at, attempts
                   FROM nest_review_jobs WHERE state = 'queued' AND next_attempt_at <= ?1
                  ORDER BY next_attempt_at LIMIT 20",
            )?;
            let rows = stmt
                .query_map([now], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    if due.is_empty() {
        return 0;
    }
    let Ok(from) = platform_mailbox(state).await else {
        tracing::warn!("review jobs waiting: no marketplace mailbox");
        return 0;
    };
    let mut n = 0;
    for (job, publisher, op, input, mailbox, deadline, attempts) in due {
        let (j, p, m) = (job.clone(), publisher.clone(), mailbox.clone());
        let owned = state
            .store
            .group_tx(move |tx| -> GResult<bool> {
                if service_owned_in(tx, &p, &m)? {
                    return Ok(true);
                }
                fail_in(
                    tx,
                    &j,
                    vec![finding(
                        "conformance.mailbox",
                        &j,
                        "the service mailbox is not the publisher's",
                    )],
                    now,
                )?;
                Ok(false)
            })
            .await
            .unwrap_or(false);
        if !owned {
            continue;
        }
        let input: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
        let body = review_job_message(&state.public_url, &job, &op, &input, deadline);
        let extras = crate::msgactions::SendExtras {
            msg_ref: Some(msg_ref_for(&job, "review")),
            ..Default::default()
        };
        let sent = crate::do_send_with(
            state,
            &from,
            &mailbox,
            &body,
            crate::ThreadRequest::Default,
            &extras,
            false,
        )
        .await;
        let outcome = match &sent {
            Ok(v) => Ok(v["message_id"].as_str().unwrap_or_default().to_string()),
            Err(e) => Err(e.code),
        };
        let (j, retry_at) = (job.clone(), now + backoff(attempts));
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                match outcome {
                    Ok(message) => tx.execute(
                        "UPDATE nest_review_jobs SET state = 'running', dispatch_message = ?2,
                            attempts = attempts + 1, last_error = NULL, updated_at = ?3
                          WHERE id = ?1 AND state = 'queued'",
                        params![j, message, now],
                    )?,
                    Err(code) => tx.execute(
                        "UPDATE nest_review_jobs SET attempts = attempts + 1, last_error = ?2,
                            next_attempt_at = ?3, updated_at = ?4
                          WHERE id = ?1 AND state = 'queued'",
                        params![j, code, retry_at, now],
                    )?,
                };
                Ok(())
            })
            .await;
        n += usize::from(sent.is_ok());
    }
    n
}

/// A review job Pigeonpost lost (never sent, its scan lost to a restart or never run, a scanner
/// failure): never the publisher's failure. The job becomes `platform_error` and a fresh one,
/// with a new id and a full deadline, replaces it, up to [`MAX_PLATFORM_RETRIES`] times per
/// operation; after that the stage waits for staff instead of failing the publisher.
fn replace_in(c: &Connection, job: &str, why: &str, now: i64) -> rusqlite::Result<bool> {
    type Old = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
    );
    let old: Option<Old> = c
        .query_row(
            "SELECT case_id, release_id, publisher_id, operation, operation_version, input,
                    service_mailbox, state, created_at, deadline_at
               FROM nest_review_jobs WHERE id = ?1",
            [job],
            |r| {
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
                ))
            },
        )
        .optional()?;
    let Some((case, release, publisher, op, ver, input, mailbox, st, created, deadline)) = old
    else {
        return Ok(false);
    };
    if matches!(st.as_str(), "passed" | "failed" | "platform_error") {
        return Ok(false);
    }
    c.execute(
        "UPDATE nest_review_jobs SET state = 'platform_error', last_error = ?2, finished_at = ?3,
            updated_at = ?3 WHERE id = ?1",
        params![job, why, now],
    )?;
    let lost: i64 = c.query_row(
        "SELECT COUNT(*) FROM nest_review_jobs WHERE case_id = ?1 AND operation = ?2
           AND operation_version = ?3 AND state = 'platform_error'",
        params![case, op, ver],
        |r| r.get(0),
    )?;
    if lost > MAX_PLATFORM_RETRIES {
        tracing::warn!(
            case,
            job,
            why,
            "conformance run keeps failing on Pigeonpost's side; staff"
        );
        return Ok(false);
    }
    c.execute(
        "INSERT INTO nest_review_jobs (id, case_id, release_id, publisher_id, operation,
            operation_version, input, service_mailbox, state, next_attempt_at, deadline_at,
            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9, ?10, ?9, ?9)",
        params![
            new_id("job"),
            case,
            release,
            publisher,
            op,
            ver,
            input,
            mailbox,
            now,
            now + (deadline - created).max(3600)
        ],
    )?;
    tracing::warn!(
        case,
        job,
        why,
        "review job lost on Pigeonpost's side; sent again"
    );
    Ok(true)
}

/// Replacements one operation's review job may have before the stage waits for staff.
pub(crate) const MAX_PLATFORM_RETRIES: i64 = 3;

/// Review jobs past their deadline without an accepted result. A job the service could not
/// answer because of Pigeonpost (never sent, its scan never done, the result refused as
/// `scan_unavailable`) is replaced; any other fails.
async fn expire_once(state: &AppState, now: i64) -> GResult<usize> {
    state
        .store
        .group_tx(move |tx| -> GResult<usize> {
            let due: Vec<(String, Option<String>, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, last_error, state FROM nest_review_jobs
                      WHERE state IN ('queued','running','scanning') AND deadline_at <= ?1
                      LIMIT 100",
                )?;
                let rows = stmt
                    .query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for (job, last, st) in &due {
                match (st.as_str(), last.as_deref()) {
                    ("queued", _) => {
                        replace_in(tx, job, "never sent before the deadline", now)?;
                    }
                    ("scanning", _) => {
                        replace_in(tx, job, "the result was not scanned before the deadline", now)?;
                    }
                    (_, Some("scan_unavailable")) => {
                        replace_in(tx, job, "the build plane could not take the result", now)?;
                    }
                    (_, Some(e)) => {
                        let f = finding(
                            "conformance.result",
                            job,
                            format!(
                                "no acceptable result before the deadline; the last was refused: {e}"
                            ),
                        );
                        fail_in(tx, job, vec![f], now)?;
                    }
                    _ => {
                        let f = finding(
                            "conformance.deadline",
                            job,
                            "no result before the operation's deadline",
                        );
                        fail_in(tx, job, vec![f], now)?;
                    }
                }
            }
            Ok(due.len())
        })
        .await
}

/// `POST /v1/nest-jobs/{job}/result` for a review job (called by the result route when the job is
/// not a buyer's). The same authentication and contract as a buyer's job.
pub(crate) async fn post_result(
    state: &AppState,
    headers: &HeaderMap,
    job: &str,
    me: &crate::store::StoredIdentity,
    body: Result<Json<Value>, JsonRejection>,
) -> GResult<(StatusCode, Json<Value>)> {
    let j = job.to_string();
    type Row = (String, String, String, Option<String>, i64);
    let row: Option<Row> = state
        .store
        .group_read(move |c| -> GResult<Option<Row>> {
            Ok(c.query_row(
                "SELECT service_mailbox, publisher_id, state, result_digest, deadline_at
                   FROM nest_review_jobs WHERE id = ?1",
                [&j],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?)
        })
        .await?;
    let Some((mailbox, publisher, st, recorded, deadline)) = row else {
        return Err(not_found());
    };
    let principal = crate::principal_for_token(state, crate::bearer(headers)).await?;
    authorize(
        &Actor::Principal(&principal),
        Action::NestOperate,
        &Resource::InstalledNest(job),
        &Facts {
            nest_permitted: mailbox == me.address,
            ..facts(state)
        },
    )
    .map_err(|d| deny(d, true))?;
    let now = now_unix() as i64;
    let req: ResultReq = parse(body)?;
    let (status, _summary, files, digest) = match checked_result(req) {
        Ok(r) => r,
        Err(e) => {
            // Answered as for a buyer's job; kept as the job's finding should nothing better come.
            let (j, code) = (job.to_string(), e.code);
            let _ = state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute(
                        "UPDATE nest_review_jobs SET last_error = ?2, updated_at = ?3
                          WHERE id = ?1 AND state = 'running'",
                        params![j, code, now],
                    )?;
                    Ok(())
                })
                .await;
            return Err(e);
        }
    };
    if let Some(r) = recorded {
        if r != digest {
            return Err(conflict_code(
                "result_recorded",
                "this job already has a different result",
            ));
        }
        return Ok((
            StatusCode::OK,
            Json(json!({ "data": review_json(job, &st), "replay": true })),
        ));
    }
    if st != "running" {
        return Err(conflict_code("job_closed", "this job is no longer open"));
    }
    let mut problems = Vec::new();
    if status != "succeeded" {
        problems.push(finding(
            "conformance.failed",
            job,
            "the service answered the review job as failed",
        ));
    }
    for (i, (_, _, bytes)) in files.iter().enumerate() {
        for p in pdf_problems(bytes) {
            problems.push(finding(
                p,
                job,
                format!("files[{i}] is not a PDF a buyer can open ({p})"),
            ));
        }
    }
    if now > deadline {
        problems.push(finding(
            "conformance.deadline",
            job,
            "the result came after the operation's deadline",
        ));
    }
    let (p, m) = (publisher.clone(), mailbox.clone());
    let owned = state
        .store
        .group_read(move |c| -> GResult<bool> { Ok(service_owned_in(c, &p, &m)?) })
        .await?;
    if !owned {
        problems.push(finding(
            "conformance.mailbox",
            job,
            "the service mailbox is no longer the publisher's",
        ));
    }
    let meta: Vec<Value> = files
        .iter()
        .map(|(name, media, bytes)| {
            json!({ "name": name, "media_type": media, "bytes": bytes.len(),
                    "sha256": digest_of(bytes) })
        })
        .collect();
    // A clean result goes to the build plane's ClamAV and YARA before it can pass.
    let scan = if problems.is_empty() && !files.is_empty() {
        let scan_id = new_id("scn");
        let body = json!({
            "scan_id": scan_id,
            "kind": "files",
            "files": files.iter().map(|(name, media, bytes)| json!({
                "name": name, "media_type": media,
                "content_b64": crate::groups::b64(bytes),
            })).collect::<Vec<_>>(),
        });
        let unavailable = || {
            GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "scan_unavailable",
                "the result cannot be scanned right now; send it again",
            )
        };
        let accepted = !matches!(state.nests.marketplace.buildd, Buildd::Disabled)
            && matches!(
                state
                    .nests
                    .marketplace
                    .buildd_call(reqwest::Method::POST, "/v1/scans", Some(body))
                    .await,
                Ok((200..=299, _))
            );
        if !accepted {
            // Pigeonpost's side: if the deadline passes like this, the job is replaced, not
            // failed (see `expire_once`).
            let j = job.to_string();
            let _ = state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute(
                        "UPDATE nest_review_jobs SET last_error = 'scan_unavailable',
                            updated_at = ?2 WHERE id = ?1 AND state = 'running'",
                        params![j, now],
                    )?;
                    Ok(())
                })
                .await;
            return Err(unavailable());
        }
        Some(scan_id)
    } else {
        None
    };
    let (j, st2) = (
        job.to_string(),
        if scan.is_some() { "scanning" } else { "failed" },
    );
    let files_text = Value::Array(meta).to_string();
    let problems_text = Value::Array(problems).to_string();
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let changed = tx.execute(
                "UPDATE nest_review_jobs SET state = ?2, result_status = ?3, result_digest = ?4,
                    files = ?5, scan_id = ?6, problems = ?7, last_error = NULL,
                    finished_at = CASE WHEN ?2 = 'failed' THEN ?8 ELSE NULL END, updated_at = ?8
                  WHERE id = ?1 AND state = 'running'",
                params![j, st2, status, digest, files_text, scan, problems_text, now],
            )?;
            if changed == 0 {
                return Err(conflict_code("job_closed", "this job is no longer open"));
            }
            Ok(())
        })
        .await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "data": review_json(job, st2), "replay": false })),
    ))
}

/// What the service reads back about its review job.
fn review_json(job: &str, st: &str) -> Value {
    json!({ "job_id": job, "review": true, "execution": st, "charge": null })
}

/// Read finished ClamAV/YARA scans of review results.
async fn poll_scans(state: &AppState, now: i64) -> usize {
    if matches!(state.nests.marketplace.buildd, Buildd::Disabled) {
        return 0;
    }
    let due: Vec<(String, String)> = state
        .store
        .group_read(|c| -> GResult<Vec<(String, String)>> {
            let mut stmt = c.prepare(
                "SELECT id, scan_id FROM nest_review_jobs WHERE state = 'scanning'
                   AND scan_id IS NOT NULL ORDER BY updated_at LIMIT 20",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let mut n = 0;
    for (job, scan) in due {
        let answer = state
            .nests
            .marketplace
            .buildd_call(reqwest::Method::GET, &format!("/v1/scans/{scan}"), None)
            .await;
        enum Outcome {
            Wait,
            Passed,
            Failed(Vec<Value>),
            Lost(&'static str),
        }
        let outcome = match answer {
            // The build plane no longer knows the scan (its queue was lost): ours to redo.
            Ok((404, _)) => Outcome::Lost("the build plane lost the result's scan"),
            Ok((200, v)) => {
                let v = if v["data"].is_object() {
                    v["data"].clone()
                } else {
                    v
                };
                match v["state"].as_str() {
                    Some("succeeded") => {
                        let blocking: Vec<Value> = v["result"]["findings"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|f| f["blocking"] == true)
                            .collect();
                        // A scanner that could not run is Pigeonpost's problem, not a verdict.
                        if blocking.iter().any(|f| {
                            f["rule"]
                                .as_str()
                                .is_some_and(|r| r.starts_with("scanner."))
                        }) {
                            Outcome::Lost("the build plane's scanners did not complete")
                        } else if blocking.is_empty() {
                            Outcome::Passed
                        } else {
                            Outcome::Failed(blocking)
                        }
                    }
                    Some("failed") => Outcome::Lost("the build plane could not scan the result"),
                    _ => Outcome::Wait,
                }
            }
            _ => Outcome::Wait,
        };
        if matches!(outcome, Outcome::Wait) {
            continue;
        }
        let j = job.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                match outcome {
                    Outcome::Passed => {
                        tx.execute(
                            "UPDATE nest_review_jobs SET state = 'passed', finished_at = ?2,
                                updated_at = ?2 WHERE id = ?1 AND state = 'scanning'",
                            params![j, now],
                        )?;
                    }
                    Outcome::Failed(found) => {
                        fail_in(tx, &j, found, now)?;
                    }
                    Outcome::Lost(why) => {
                        replace_in(tx, &j, why, now)?;
                    }
                    Outcome::Wait => {}
                }
                Ok(())
            })
            .await;
        n += 1;
    }
    n
}

/// Record the stage of every case whose review jobs have all ended.
fn finish_in(tx: &Transaction<'_>, platform_owner: Option<&str>, now: i64) -> GResult<usize> {
    let cases: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT j.case_id FROM nest_review_jobs j
              JOIN review_cases c ON c.id = j.case_id
              WHERE c.status = 'open'
                AND NOT EXISTS (SELECT 1 FROM nest_review_jobs o WHERE o.case_id = j.case_id
                                  AND o.state NOT IN ('passed','failed','platform_error'))
                AND EXISTS (SELECT 1 FROM nest_review_jobs v WHERE v.case_id = j.case_id
                              AND v.state IN ('passed','failed'))
                AND NOT EXISTS (SELECT 1 FROM review_decisions d WHERE d.case_id = j.case_id
                                  AND d.stage = 'behavioral_test')
              LIMIT 20",
        )?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for case in &cases {
        type JobRow = (
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            i64,
            i64,
            Option<i64>,
        );
        let jobs: Vec<JobRow> = {
            let mut stmt = tx.prepare(
                "SELECT id, operation, state, problems, result_digest, COALESCE(files, '[]'),
                        created_at, deadline_at, finished_at
                   FROM nest_review_jobs WHERE case_id = ?1 ORDER BY created_at, rowid",
            )?;
            let rows = stmt
                .query_map([case], |r| {
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
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        // The newest job of each operation decides; one Pigeonpost lost for good (replacements
        // used up) leaves the stage to staff rather than failing the publisher.
        let mut latest: std::collections::BTreeMap<&str, &str> = Default::default();
        for j in &jobs {
            latest.insert(j.1.as_str(), j.2.as_str());
        }
        if latest.values().any(|st| *st == "platform_error") {
            continue;
        }
        let mut findings: Vec<Value> = Vec::new();
        let mut facts = Vec::new();
        for (id, op, st, problems, digest, files, created, deadline, finished) in &jobs {
            if st != "platform_error" {
                let p: Vec<Value> = serde_json::from_str(problems).unwrap_or_default();
                if st == "failed" && p.is_empty() {
                    findings.push(finding("conformance.failed", id, "the review job failed"));
                }
                findings.extend(p);
            }
            facts.push(json!({
                "job_id": id, "operation": op, "state": st, "result_digest": digest,
                "files": serde_json::from_str::<Value>(files).unwrap_or(Value::Null),
                "seconds": finished.map(|f| f - created),
                "deadline_at": iso(*deadline),
            }));
        }
        let passed = latest.values().all(|st| *st == "passed") && findings.is_empty();
        let decision = if passed { "passed" } else { "failed" };
        let reason = if passed {
            format!("{} review job(s) answered and conformant", latest.len())
        } else {
            format!("{} conformance finding(s)", findings.len())
        };
        let evidence = canonical_json(&json!({
            "stage": "behavioral_test",
            "decision": decision,
            "reason": reason,
            "findings": findings,
            "evidence": {
                "kind": "hosted_conformance",
                "review_jobs": facts,
                "charged": false,
                "checks": ["result_shape", "pdf_validity", "clamav", "yara", "deadline",
                           "mailbox_ownership"],
            },
        }));
        let id = new_id("dec");
        tx.execute(
            "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at,
                stage, evidence, evidence_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'behavioral_test', ?7, ?8)",
            params![
                id,
                case,
                CONFORMANCE,
                decision,
                reason,
                now,
                evidence,
                digest_of(evidence.as_bytes())
            ],
        )?;
        tx.execute(
            "UPDATE review_cases SET updated_at = ?2, revision = revision + 1,
                status = CASE WHEN ?3 THEN 'blocked' ELSE status END WHERE id = ?1",
            params![case, now, !passed],
        )?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM review_cases WHERE id = ?1",
            [case],
            |r| r.get(0),
        )?;
        crate::marketplace_hardening::review_event_in(
            tx,
            case,
            "stage_changed",
            json!({ "stage": "behavioral_test", "decision": decision, "status": decision,
                    "revision": revision }),
            platform_owner,
            now,
        )?;
        if !passed {
            crate::marketplace_hardening::review_event_in(
                tx,
                case,
                "blocked",
                json!({ "stage": "behavioral_test", "blocker_code": "stage_failed",
                        "revision": revision }),
                platform_owner,
                now,
            )?;
        }
    }
    Ok(cases.len())
}

/// One pass: start, send, expire, read scans, record. Called from the hosted-job worker.
pub(crate) async fn work_once(state: &AppState) -> usize {
    let now = now_unix() as i64;
    let started = state
        .store
        .group_tx(move |tx| start_in(tx, now))
        .await
        .unwrap_or(0);
    let sent = dispatch_once(state, now).await;
    let expired = expire_once(state, now).await.unwrap_or(0);
    let scanned = poll_scans(state, now).await;
    let owner = state.platform_owner.as_deref().map(str::to_string);
    let recorded = state
        .store
        .group_tx(move |tx| finish_in(tx, owner.as_deref(), now))
        .await
        .unwrap_or(0);
    if recorded > 0 {
        crate::marketplace_hardening::wake_new_events(state).await;
    }
    started + sent + expired + scanned + recorded
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const GOOD_PDF: &[u8] =
        b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\nxref\n0 3\ntrailer << /Size 3 /Root 1 0 R >>\n\
startxref\n9\n%%EOF\n";

    #[test]
    fn pdf_structure_is_checked_beyond_the_header() {
        assert!(
            pdf_problems(GOOD_PDF).is_empty(),
            "{:?}",
            pdf_problems(GOOD_PDF)
        );
        let bare = b"%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n";
        assert_eq!(pdf_problems(bare), vec!["pdf.xref", "pdf.root"]);
        let mut enc = GOOD_PDF.to_vec();
        enc.splice(10..10, b"/Encrypt 5 0 R ".iter().copied());
        assert!(pdf_problems(&enc).contains(&"pdf.encrypted"));
        assert!(pdf_problems(b"<html></html>").contains(&"pdf.header"));
    }

    #[test]
    fn review_input_fills_every_field_within_its_limit() {
        let op = json!({ "fields": [
            { "name": "role", "max_chars": 10, "required": true },
            { "name": "notes", "max_chars": 2000, "required": false },
        ]});
        let v = review_input(&op);
        assert_eq!(v["role"].as_str().unwrap().chars().count(), 10);
        assert_eq!(v["notes"], REVIEW_INPUT);
    }
}
