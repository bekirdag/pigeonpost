//! Marketplace M2b: hosted test Nests (build plan R-70, OD-9). Staging postbox only.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET\|POST /v1/publishers/{publisher}/test-nests` | `PublisherManage` | read: the team; create: a certified publisher's admins |
//! | `GET\|DELETE /v1/publishers/{publisher}/test-nests/{test_nest}` | `PublisherManage` | read: the team; delete: the admins |
//! | `POST /v1/publishers/{publisher}/test-nests/{test_nest}/reset` | `PublisherManage` | the admins |
//! | `GET\|POST /v1/publishers/{publisher}/test-nests/{test_nest}/jobs`, `GET …/jobs/{job}` | `PublisherManage` | read: the team; invoke: the admins |
//! | `GET /v1/publishers/{publisher}/test-nests/{test_nest}/logs` | `PublisherManage` | the team |
//!
//! Every route answers `404 staging_only` unless `POSTBOX_STAGING_MODE=1`. A test Nest runs a
//! release of the publisher's that **passed intake** and failed no later stage (it need not be
//! approved) in the **test pool** (`POSTBOX_MARKETPLACE_TEST_NESTS_URL`, a nestd whose marketplace
//! pool has `test_pool = true`), never the production pool. Admission there is a test statement
//! (`pigeonpost.nest.release.test.v1`), which a production pool refuses and a test pool requires.
//! Jobs are synthetic: the publisher's own payloads, no quote, no payment, no buyer. A test Nest
//! expires after `POSTBOX_MARKETPLACE_TEST_NEST_DAYS` (default 7): a day before, the team gets
//! `nest.test.expiring`; at expiry its worker and volume are removed and `nest.test.expired` sent.
//! Reset removes the worker and its data and starts the next job on a fresh one.

use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, iso, not_found, parse};
use crate::marketplace_env::{staging_only, test_pool};
use crate::marketplace_m1::{
    canonical_json, digest_of, operation_ok, validate_payload, version_ok,
};
use crate::marketplace_m2::{certified, gate, new_id, release_in, text, wake, Need, PAGE};
use crate::policy::Action;
use crate::store::marketplace as store;
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// The admission statement type a test pool accepts and a production pool refuses.
pub(crate) const TEST_STATEMENT_TYPE: &str = "pigeonpost.nest.release.test.v1";
/// Live test Nests one publisher may hold.
const MAX_LIVE: i64 = 5;
/// Synthetic jobs one test Nest may take per day.
const MAX_JOBS_PER_DAY: i64 = 500;
/// `nest.test.expiring` goes out this long before expiry.
const WARN_SECS: i64 = 86_400;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publishers/{publisher}/test-nests",
            get(list).post(create),
        )
        .route(
            "/v1/publishers/{publisher}/test-nests/{test_nest}",
            get(read).delete(remove),
        )
        .route(
            "/v1/publishers/{publisher}/test-nests/{test_nest}/reset",
            post(reset),
        )
        .route(
            "/v1/publishers/{publisher}/test-nests/{test_nest}/jobs",
            get(list_jobs).post(invoke),
        )
        .route(
            "/v1/publishers/{publisher}/test-nests/{test_nest}/jobs/{job}",
            get(read_job),
        )
        .route(
            "/v1/publishers/{publisher}/test-nests/{test_nest}/logs",
            get(logs),
        )
}

/// The buyer context of one generation of a test Nest: a reset starts a new one.
fn context_of(test_nest: &str, generation: i64) -> String {
    let d = digest_of(format!("pigeonpost-test-nest\0{test_nest}\0{generation}").as_bytes());
    format!("ctx-t{}", &d.trim_start_matches("sha256:")[..24])
}

/// The test admission statement for a release in one test Nest.
pub(crate) fn test_statement(
    release_id: &str,
    release_digest: &str,
    image: &str,
    test_nest: &str,
    expires_at: i64,
    key_id: &str,
) -> String {
    canonical_json(&json!({
        "type": TEST_STATEMENT_TYPE,
        "key_id": key_id,
        "release_id": release_id,
        "release_digest": release_digest,
        "image": image,
        "image_digest": image.split_once('@').map(|(_, d)| d).unwrap_or(""),
        "decision": "test_only",
        "environment": "staging",
        "test_nest_id": test_nest,
        "expires_at": iso(expires_at),
    }))
}

fn event_in(
    c: &Connection,
    test_nest: &str,
    kind: &str,
    detail: Value,
    now: i64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO nest_test_events (test_nest_id, kind, detail, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![test_nest, kind, detail.to_string(), now],
    )?;
    Ok(())
}

const ENV_COLS: &str =
    "t.id, t.publisher_id, t.release_id, t.build_id, t.label, t.generation, t.status, \
     t.created_at, t.updated_at, t.expires_at, t.ended_at, r.version";

fn env_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "publisher_id": r.get::<_, String>(1)?,
        "release_id": r.get::<_, String>(2)?,
        "build_id": r.get::<_, Option<String>>(3)?,
        "label": r.get::<_, String>(4)?,
        "generation": r.get::<_, i64>(5)?,
        "status": r.get::<_, String>(6)?,
        "created_at": iso(r.get(7)?),
        "updated_at": iso(r.get(8)?),
        "expires_at": iso(r.get(9)?),
        "ended_at": r.get::<_, Option<i64>>(10)?.map(iso),
        "version": r.get::<_, String>(11)?,
        "environment": "staging",
    }))
}

fn env_in(c: &Connection, id: &str, publisher: &str) -> rusqlite::Result<Option<Value>> {
    c.query_row(
        &format!(
            "SELECT {ENV_COLS} FROM nest_test_envs t JOIN nest_releases r ON r.id = t.release_id
              WHERE t.id = ?1 AND t.publisher_id = ?2"
        ),
        params![id, publisher],
        env_json,
    )
    .optional()
}

/// Staging first (the routes do not exist elsewhere), then the publisher gate.
async fn open(
    state: &AppState,
    headers: &HeaderMap,
    publisher: &str,
    need: Need,
) -> GResult<crate::marketplace_m2::Ctx> {
    staging_only(state)?;
    let ctx = gate(state, headers, publisher, Action::PublisherManage, need).await?;
    if need == Need::Admin {
        certified(&ctx.p)?;
    }
    Ok(ctx)
}

/// `GET /v1/publishers/{publisher}/test-nests`
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Read).await?;
        let pid = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {ENV_COLS} FROM nest_test_envs t JOIN nest_releases r ON r.id = t.release_id
                      WHERE t.publisher_id = ?1 ORDER BY t.status = 'active' DESC, t.created_at DESC, t.id
                      LIMIT {PAGE}"
                ))?;
                let rows = stmt
                    .query_map([&pid], env_json)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    #[serde(default)]
    release_id: Option<String>,
    #[serde(default)]
    build_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    /// Shorter than the configured lifetime, never longer.
    #[serde(default)]
    days: Option<u64>,
}

/// Why a release cannot be tested, or `None` when it can: intake passed, no stage failed, not
/// rejected, revoked or quarantined, and built (image and digest present).
fn untestable_in(
    c: &Connection,
    release: &crate::marketplace_m2::Rel,
) -> rusqlite::Result<Option<GErr>> {
    if release.security_state != "normal"
        || matches!(release.status.as_str(), "rejected" | "revoked")
    {
        return Ok(Some(GErr::conflict(
            "release_not_testable",
            "this release is rejected, revoked or quarantined",
        )));
    }
    if release.image.is_none() || release.release_digest.is_none() {
        return Ok(Some(GErr::conflict(
            "release_not_testable",
            "this release was not built here",
        )));
    }
    let (intake, failed): (bool, bool) = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM review_decisions d JOIN review_cases k ON k.id = d.case_id
                         WHERE k.release_id = ?1 AND d.stage = 'intake' AND d.decision = 'passed'),
                EXISTS (SELECT 1 FROM review_decisions d JOIN review_cases k ON k.id = d.case_id
                         WHERE k.release_id = ?1 AND d.decision = 'failed')",
        [&release.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !intake {
        return Ok(Some(GErr::conflict(
            "intake_required",
            "a test Nest runs a release that passed intake",
        )));
    }
    if failed {
        return Ok(Some(GErr::conflict(
            "release_not_testable",
            "a review stage of this release failed",
        )));
    }
    Ok(None)
}

/// `POST /v1/publishers/{publisher}/test-nests` — a test Nest from a build or a release.
async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Admin).await?;
        test_pool(&state)?;
        let req: CreateReq = parse(body)?;
        let label = match &req.label {
            Some(l) => text(l, "label", 1, 80)?,
            None => "test".to_string(),
        };
        let max_secs = state.nests.mp.test_nest_secs as i64;
        let secs = match req.days {
            None => max_secs,
            Some(d) if d >= 1 && (d as i64) * 86_400 <= max_secs => d as i64 * 86_400,
            Some(_) => {
                return Err(GErr::bad(
                    "invalid_request",
                    format!("days: 1 to {}", max_secs / 86_400),
                ))
            }
        };
        let now = now_unix() as i64;
        let id = new_id("tnt");
        let (tid, pid, by) = (id.clone(), ctx.p.id.clone(), ctx.account.clone());
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let (release, build) = match (&req.release_id, &req.build_id) {
                    (Some(r), None) => (r.clone(), None),
                    (None, Some(b)) => {
                        let rel: Option<Option<String>> = tx
                            .query_row(
                                "SELECT release_id FROM nest_builds WHERE id = ?1 AND publisher_id = ?2",
                                params![b, pid],
                                |r| r.get(0),
                            )
                            .optional()?;
                        match rel {
                            None => return Err(not_found()),
                            Some(None) => {
                                return Err(GErr::conflict("build_not_built", "this build has no release yet"))
                            }
                            Some(Some(r)) => (r, Some(b.clone())),
                        }
                    }
                    _ => return Err(GErr::bad("invalid_request", "either release_id or build_id")),
                };
                let rel = release_in(tx, &release)?
                    .filter(|r| r.publisher_id == pid)
                    .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such release"))?;
                if let Some(e) = untestable_in(tx, &rel)? {
                    return Err(e);
                }
                let live: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM nest_test_envs WHERE publisher_id = ?1 AND status = 'active'",
                    [&pid],
                    |r| r.get(0),
                )?;
                if live >= MAX_LIVE {
                    return Err(GErr::conflict("too_many_test_nests", "delete a test Nest first"));
                }
                tx.execute(
                    "INSERT INTO nest_test_envs (id, publisher_id, release_id, build_id, label, buyer_context,
                        generation, status, created_by, created_at, updated_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'active', ?7, ?8, ?8, ?9)",
                    params![tid, pid, rel.id, build, label, context_of(&tid, 1), by, now, now + secs],
                )?;
                event_in(tx, &tid, "created", json!({ "release_id": rel.id, "expires_at": iso(now + secs) }), now)?;
                let w = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.test.changed",
                    &json!({ "test_nest_id": tid, "status": "active" }),
                    now as u64,
                )?;
                Ok((env_in(tx, &tid, &pid)?.unwrap_or(Value::Null), w))
            })
            .await?;
        wake(&state, wake_list);
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}/test-nests/{test_nest}`
async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Read).await?;
        let pid = ctx.p.id.clone();
        let v = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> { Ok(env_in(c, &test_nest, &pid)?) })
            .await?
            .ok_or_else(not_found)?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// A live test Nest of this publisher: `(release, buyer context, generation, expires_at)`.
fn live_in(
    c: &Connection,
    id: &str,
    publisher: &str,
    now: i64,
) -> GResult<(String, String, i64, i64)> {
    let row: Option<(String, String, i64, String, i64)> = c
        .query_row(
            "SELECT release_id, buyer_context, generation, status, expires_at FROM nest_test_envs
              WHERE id = ?1 AND publisher_id = ?2",
            params![id, publisher],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((release, context, generation, status, expires)) = row else {
        return Err(not_found());
    };
    if status != "active" || expires <= now {
        return Err(GErr::conflict(
            "test_nest_ended",
            "this test Nest expired or was deleted",
        ));
    }
    Ok((release, context, generation, expires))
}

/// Ask the test pool to delete one worker and its volume. A pool that cannot be reached is
/// reported, not hidden.
async fn remove_worker(state: &AppState, test_nest: &str, context: &str) -> bool {
    let Some(pool) = state.nests.mp.test_pool.clone() else {
        return false;
    };
    matches!(
        pool.call(
            reqwest::Method::POST,
            "/v1/mp/workers/remove",
            Some(json!({ "listing_id": test_nest, "buyer_context": context })),
        )
        .await,
        Ok((200, _))
    )
}

/// End a test Nest (`deleted` or `expired`): stop its open jobs and record it. Returns the old
/// buyer context, or `None` when it had already ended.
fn end_in(
    c: &Connection,
    id: &str,
    status: &str,
    now: i64,
) -> rusqlite::Result<Option<(String, String)>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT buyer_context, publisher_id FROM nest_test_envs WHERE id = ?1 AND status = 'active'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((context, publisher)) = row else {
        return Ok(None);
    };
    c.execute(
        "UPDATE nest_test_envs SET status = ?2, ended_at = ?3, updated_at = ?3 WHERE id = ?1",
        params![id, status, now],
    )?;
    c.execute(
        "UPDATE nest_test_jobs SET state = 'stopped', error_code = ?2, updated_at = ?3
          WHERE test_nest_id = ?1 AND state IN ('queued','running')",
        params![id, status, now],
    )?;
    event_in(c, id, status, json!({}), now)?;
    Ok(Some((context, publisher)))
}

/// `DELETE /v1/publishers/{publisher}/test-nests/{test_nest}`
async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Admin).await?;
        let pid = ctx.p.id.clone();
        let now = now_unix() as i64;
        let tid = test_nest.clone();
        let (context, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(String, Vec<String>)> {
                let owned: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM nest_test_envs WHERE id = ?1 AND publisher_id = ?2)",
                    params![tid, pid],
                    |r| r.get(0),
                )?;
                if !owned {
                    return Err(not_found());
                }
                let Some((context, _)) = end_in(tx, &tid, "deleted", now)? else {
                    return Err(GErr::conflict("test_nest_ended", "this test Nest already ended"));
                };
                let w = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.test.changed",
                    &json!({ "test_nest_id": tid, "status": "deleted" }),
                    now as u64,
                )?;
                Ok((context, w))
            })
            .await?;
        wake(&state, wake_list);
        let removed = remove_worker(&state, &test_nest, &context).await;
        Ok(Json(json!({ "data": { "id": test_nest, "status": "deleted", "runtime_removed": removed } })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/publishers/{publisher}/test-nests/{test_nest}/reset` — delete the worker and its
/// data; the next job starts on a fresh one. The expiry does not move.
async fn reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Admin).await?;
        test_pool(&state)?;
        let pid = ctx.p.id.clone();
        let now = now_unix() as i64;
        let tid = test_nest.clone();
        let old = state
            .store
            .group_tx(move |tx| -> GResult<String> {
                let (_, context, generation, _) = live_in(tx, &tid, &pid, now)?;
                let next = generation + 1;
                tx.execute(
                    "UPDATE nest_test_envs SET generation = ?2, buyer_context = ?3, updated_at = ?4
                      WHERE id = ?1",
                    params![tid, next, context_of(&tid, next), now],
                )?;
                tx.execute(
                    "UPDATE nest_test_jobs SET state = 'stopped', error_code = 'reset', updated_at = ?2
                      WHERE test_nest_id = ?1 AND state IN ('queued','running')",
                    params![tid, now],
                )?;
                event_in(tx, &tid, "reset", json!({ "generation": next }), now)?;
                Ok(context)
            })
            .await?;
        let removed = remove_worker(&state, &test_nest, &old).await;
        let pid = ctx.p.id.clone();
        let v = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> { Ok(env_in(c, &test_nest, &pid)?) })
            .await?
            .unwrap_or(Value::Null);
        Ok(Json(json!({ "data": v, "runtime_removed": removed })))
    }
    .await;
    into_response(result)
}

const JOB_COLS: &str =
    "id, test_nest_id, generation, operation, operation_version, state, result, \
     error_code, created_at, updated_at";

fn job_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "test_nest_id": r.get::<_, String>(1)?,
        "generation": r.get::<_, i64>(2)?,
        "operation": r.get::<_, String>(3)?,
        "operation_version": r.get::<_, String>(4)?,
        "state": r.get::<_, String>(5)?,
        "result": r.get::<_, Option<String>>(6)?.and_then(|t| serde_json::from_str::<Value>(&t).ok()),
        "error_code": r.get::<_, Option<String>>(7)?,
        "created_at": iso(r.get(8)?),
        "updated_at": iso(r.get(9)?),
        "synthetic": true,
    }))
}

/// `GET /v1/publishers/{publisher}/test-nests/{test_nest}/jobs`
async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Read).await?;
        let pid = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                if env_in(c, &test_nest, &pid)?.is_none() {
                    return Err(not_found());
                }
                let mut stmt = c.prepare(&format!(
                    "SELECT {JOB_COLS} FROM nest_test_jobs WHERE test_nest_id = ?1
                      ORDER BY created_at DESC, id LIMIT {PAGE}"
                ))?;
                let rows = stmt
                    .query_map([&test_nest], job_json)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobReq {
    operation: String,
    operation_version: String,
    payload: Map<String, Value>,
}

/// Hand a queued test job to the test pool. A transport failure leaves it queued.
async fn dispatch(state: &AppState, job: &str) -> GResult<()> {
    let pool = test_pool(state)?;
    let jid = job.to_string();
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    );
    let row: Option<Row> = state
        .store
        .group_read(move |c| -> GResult<Option<Row>> {
            Ok(c.query_row(
                "SELECT j.test_nest_id, t.buyer_context, j.operation, j.operation_version, j.payload,
                        r.id, r.image, r.release_digest, r.manifest, t.expires_at
                   FROM nest_test_jobs j JOIN nest_test_envs t ON t.id = j.test_nest_id
                   JOIN nest_releases r ON r.id = t.release_id
                  WHERE j.id = ?1 AND j.state = 'queued' AND t.status = 'active'
                    AND j.generation = t.generation",
                [&jid],
                |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                        r.get(7)?, r.get(8)?, r.get(9)?,
                    ))
                },
            )
            .optional()?)
        })
        .await?;
    let Some((
        tid,
        context,
        op,
        ver,
        payload,
        release,
        Some(image),
        Some(digest),
        manifest,
        expires,
    )) = row
    else {
        return Ok(());
    };
    // Signed like an attestation (hardening): the test pool trusts the staging postbox's key.
    let (key, key_id) = state.nests.marketplace.signer()?;
    let statement = test_statement(&release, &digest, &image, &tid, expires, &key_id);
    let signature = crate::marketplace_hardening::sign(&key, &statement);
    let body = json!({
        "job_id": job,
        "listing_id": tid,
        "buyer_context": context,
        "operation": op,
        "operation_version": ver,
        "payload": serde_json::from_str::<Value>(&payload).unwrap_or(json!({})),
        "release": {
            "release_id": release,
            "image": image,
            "release_digest": digest,
            "statement_digest": digest_of(statement.as_bytes()),
            "statement": statement,
            "signature": signature,
            "key_id": key_id,
        },
        "config": crate::marketplace_m2::delivered_config(manifest.as_deref(), Some("{}")),
    });
    let (state_next, worker, error): (&str, Option<String>, Option<String>) = match pool
        .call(reqwest::Method::POST, "/v1/mp/jobs", Some(body))
        .await
    {
        Ok((200, v)) => ("running", v["worker"].as_str().map(str::to_string), None),
        Ok((status, v)) if (400..500).contains(&status) => (
            "failed",
            None,
            Some(
                v["error"]
                    .as_str()
                    .unwrap_or("dispatch_refused")
                    .to_string(),
            ),
        ),
        _ => return Ok(()),
    };
    let now = now_unix() as i64;
    let jid = job.to_string();
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let n = tx.execute(
                "UPDATE nest_test_jobs SET state = ?2, worker = ?3, error_code = ?4, updated_at = ?5
                  WHERE id = ?1 AND state = 'queued'",
                params![jid, state_next, worker, error, now],
            )?;
            if n > 0 {
                event_in(
                    tx,
                    &tid,
                    "job",
                    json!({ "job_id": jid, "state": state_next, "error_code": error }),
                    now,
                )?;
            }
            Ok(())
        })
        .await
}

/// Collect a running test job's result from the test pool.
async fn poll(state: &AppState, job: &str, test_nest: &str) -> GResult<()> {
    let Some(pool) = state.nests.mp.test_pool.clone() else {
        return Ok(());
    };
    let Ok((200, v)) = pool
        .call(reqwest::Method::GET, &format!("/v1/mp/jobs/{job}"), None)
        .await
    else {
        return Ok(());
    };
    let next = match v["state"].as_str() {
        Some("succeeded") => "succeeded",
        Some("failed") => "failed",
        Some("stopped_security") => "stopped",
        _ => return Ok(()),
    };
    let result = (next == "succeeded").then(|| v["result"].to_string());
    let error = (next != "succeeded").then(|| {
        v["result"]["error_code"]
            .as_str()
            .unwrap_or(next)
            .to_string()
    });
    let now = now_unix() as i64;
    let (jid, tid) = (job.to_string(), test_nest.to_string());
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let n = tx.execute(
                "UPDATE nest_test_jobs SET state = ?2, result = ?3, error_code = ?4, updated_at = ?5
                  WHERE id = ?1 AND state = 'running'",
                params![jid, next, result, error, now],
            )?;
            if n > 0 {
                event_in(
                    tx,
                    &tid,
                    "job",
                    json!({ "job_id": jid, "state": next, "error_code": error }),
                    now,
                )?;
            }
            Ok(())
        })
        .await
}

/// `POST /v1/publishers/{publisher}/test-nests/{test_nest}/jobs` — one synthetic job: the
/// operation's input schema is enforced, nothing is quoted or paid.
async fn invoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Admin).await?;
        test_pool(&state)?;
        let req: JobReq = parse(body)?;
        if !operation_ok(&req.operation) || !version_ok(&req.operation_version) {
            return Err(GErr::bad("invalid_request", "operation or version malformed"));
        }
        let now = now_unix() as i64;
        let (pid, tid, by) = (ctx.p.id.clone(), test_nest.clone(), ctx.account.clone());
        let id = format!("job_{}", crate::groups::rand_b32(16));
        let jid = id.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let (release, _, generation, _) = live_in(tx, &tid, &pid, now)?;
                let rel = release_in(tx, &release)?.ok_or_else(not_found)?;
                let schema = crate::marketplace_m2::operation_schema(
                    &rel.manifest,
                    &req.operation,
                    &req.operation_version,
                )
                .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such operation"))?;
                validate_payload(&schema, &req.payload)?;
                let today: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM nest_test_jobs WHERE test_nest_id = ?1 AND created_at > ?2",
                    params![tid, now - 86_400],
                    |r| r.get(0),
                )?;
                if today >= MAX_JOBS_PER_DAY {
                    return Err(GErr::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many test jobs today"));
                }
                tx.execute(
                    "INSERT INTO nest_test_jobs (id, test_nest_id, generation, operation, operation_version,
                        payload, state, created_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?8)",
                    params![jid, tid, generation, req.operation, req.operation_version,
                            Value::Object(req.payload.clone()).to_string(), by, now],
                )?;
                event_in(tx, &tid, "job", json!({ "job_id": jid, "state": "queued" }), now)?;
                Ok(())
            })
            .await?;
        dispatch(&state, &id).await?;
        let (jid, tid) = (id.clone(), test_nest.clone());
        let v = state
            .store
            .group_read(move |c| -> GResult<Value> {
                Ok(c.query_row(
                    &format!("SELECT {JOB_COLS} FROM nest_test_jobs WHERE id = ?1 AND test_nest_id = ?2"),
                    params![jid, tid],
                    job_json,
                )?)
            })
            .await?;
        Ok((StatusCode::ACCEPTED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}/test-nests/{test_nest}/jobs/{job}` — retries a queued job's
/// hand-over and collects a running one's result.
async fn read_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest, job)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Read).await?;
        let pid = ctx.p.id.clone();
        let (tid, jid) = (test_nest.clone(), job.clone());
        let current: Option<String> = state
            .store
            .group_read(move |c| -> GResult<Option<String>> {
                Ok(c.query_row(
                    "SELECT j.state FROM nest_test_jobs j JOIN nest_test_envs t ON t.id = j.test_nest_id
                      WHERE j.id = ?1 AND j.test_nest_id = ?2 AND t.publisher_id = ?3",
                    params![jid, tid, pid],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await?;
        match current.as_deref() {
            None => return Err(not_found()),
            Some("queued") => dispatch(&state, &job).await?,
            Some("running") => poll(&state, &job, &test_nest).await?,
            _ => {}
        }
        let (jid, tid) = (job.clone(), test_nest.clone());
        let v = state
            .store
            .group_read(move |c| -> GResult<Value> {
                Ok(c.query_row(
                    &format!("SELECT {JOB_COLS} FROM nest_test_jobs WHERE id = ?1 AND test_nest_id = ?2"),
                    params![jid, tid],
                    job_json,
                )?)
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}/test-nests/{test_nest}/logs` — the test Nest's event log:
/// ids, states and error codes, oldest first.
async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, test_nest)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = open(&state, &headers, &publisher, Need::Read).await?;
        let pid = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                if env_in(c, &test_nest, &pid)?.is_none() {
                    return Err(not_found());
                }
                let mut stmt = c.prepare(
                    "SELECT id, kind, detail, created_at FROM nest_test_events WHERE test_nest_id = ?1
                      ORDER BY id LIMIT 1000",
                )?;
                let rows = stmt
                    .query_map([&test_nest], |r| {
                        Ok(json!({
                            "seq": r.get::<_, i64>(0)?,
                            "kind": r.get::<_, String>(1)?,
                            "detail": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
                            "at": iso(r.get(3)?),
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// Ended test Nests (id, old buyer context) and the accounts to wake.
type Ended = (Vec<(String, String)>, Vec<String>);

/// One expiry pass: warn a day ahead (`nest.test.expiring`), end what has expired
/// (`nest.test.expired`) and remove its worker. Returns how many ended.
pub(crate) async fn sweep_once(state: &AppState, now: i64) -> usize {
    let warned = state
        .store
        .group_tx(move |tx| -> GResult<Vec<String>> {
            let due: Vec<(String, String, i64)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, publisher_id, expires_at FROM nest_test_envs
                      WHERE status = 'active' AND warned_at IS NULL AND expires_at - ?1 <= ?2
                        AND expires_at > ?1 LIMIT 100",
                )?;
                let rows = stmt
                    .query_map(params![now, WARN_SECS], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut woken = Vec::new();
            for (id, publisher, expires) in due {
                tx.execute(
                    "UPDATE nest_test_envs SET warned_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                event_in(
                    tx,
                    &id,
                    "expiring",
                    json!({ "expires_at": iso(expires) }),
                    now,
                )?;
                woken.extend(store::publisher_event_in(
                    tx,
                    &publisher,
                    "nest.test.expiring",
                    &json!({ "test_nest_id": id, "expires_at": iso(expires) }),
                    now as u64,
                )?);
            }
            Ok(woken)
        })
        .await
        .unwrap_or_default();
    wake(state, warned);
    let ended = state
        .store
        .group_tx(move |tx| -> GResult<Ended> {
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM nest_test_envs WHERE status = 'active' AND expires_at <= ?1 LIMIT 100",
                )?;
                let rows = stmt
                    .query_map([now], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                rows
            };
            let mut out = Vec::new();
            let mut woken = Vec::new();
            for id in ids {
                if let Some((context, publisher)) = end_in(tx, &id, "expired", now)? {
                    woken.extend(store::publisher_event_in(
                        tx,
                        &publisher,
                        "nest.test.expired",
                        &json!({ "test_nest_id": id }),
                        now as u64,
                    )?);
                    out.push((id, context));
                }
            }
            Ok((out, woken))
        })
        .await;
    let Ok((ended, woken)) = ended else {
        return 0;
    };
    wake(state, woken);
    for (id, context) in &ended {
        remove_worker(state, id, context).await;
    }
    ended.len()
}

/// Expire test Nests every minute, on a staging postbox only.
pub(crate) fn start_sweeper(state: AppState) {
    if !state.nests.mp.staging {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            sweep_once(&state, now_unix() as i64).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_are_valid_and_change_on_reset() {
        let a = context_of("tnt_x", 1);
        let b = context_of("tnt_x", 2);
        assert_ne!(a, b);
        assert!(a.len() <= 64 && a.starts_with("ctx-t"));
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_'));
    }

    #[test]
    fn test_statements_never_read_as_approvals() {
        let s = test_statement(
            "rel_1",
            "sha256:aa",
            "img@sha256:bb",
            "tnt_1",
            0,
            "ppk_0000000000000000",
        );
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["type"], TEST_STATEMENT_TYPE);
        assert_eq!(v["decision"], "test_only");
        assert_ne!(v["type"], crate::marketplace_m1::STATEMENT_TYPE);
        assert_eq!(v["image_digest"], "sha256:bb");
    }
}
