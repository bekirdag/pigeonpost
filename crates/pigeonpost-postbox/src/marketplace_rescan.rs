//! The monthly re-scan of published releases (developer program plan §4, "After publication").
//!
//! Every `POSTBOX_MARKETPLACE_RESCAN_DAYS` (30 by default; `0` switches it off), each approved,
//! built release that a published listing names is sent to the build plane again
//! (`POST /v1/scans`, `kind: release`): OSV-Scanner and Grype on its SBOM against the newest
//! databases, and Grype, ClamAV and YARA on its image (the build plane keeps the images of
//! releases it is asked to re-scan). The scan runs offline in gVisor like the review's own.
//!
//! A **blocking** finding whose id the release's review (or an earlier flag) did not already have
//! **flags the release for staff** (`release_rescan_flags`). Nothing else happens: the release is
//! never quarantined, revoked or unlisted by a re-scan. Staff look at the flag, act with the
//! existing quarantine and revocation routes when they decide to, and acknowledge it.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/staff/marketplace/rescan-flags[?all=true]` | `NestReview` | staff with step-up |
//! | `POST /v1/staff/marketplace/rescan-flags/{flag}/acknowledge` | `NestReview` | staff with step-up |

use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, iso, not_found};
use crate::marketplace_hardening::Buildd;
use crate::marketplace_m2::{new_id, staff_gate};
use crate::policy::Action;
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

/// Days between sweeps unless `POSTBOX_MARKETPLACE_RESCAN_DAYS` says otherwise.
const DEFAULT_DAYS: i64 = 30;
/// Releases submitted per pass, and scans read per pass.
const BATCH: usize = 20;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/staff/marketplace/rescan-flags", get(list_flags))
        .route(
            "/v1/staff/marketplace/rescan-flags/{flag}/acknowledge",
            post(acknowledge),
        )
}

fn interval_secs() -> i64 {
    std::env::var("POSTBOX_MARKETPLACE_RESCAN_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| (0..=365).contains(d))
        .unwrap_or(DEFAULT_DAYS)
        * 86_400
}

/// The releases a sweep re-scans: approved, built, not revoked, named by a published listing,
/// and not re-scanned within `interval`.
pub(crate) fn due_in(
    c: &Connection,
    now: i64,
    interval: i64,
) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut stmt = c.prepare(
        "SELECT DISTINCT r.id, r.image_digest, r.sbom FROM nest_releases r
           JOIN nest_listings l ON l.release_id = r.id
          WHERE l.status = 'published' AND r.status = 'approved'
            AND r.security_state <> 'revoked'
            AND COALESCE(r.build_isolation, '') <> 'publisher_hosted'
            AND r.sbom IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM marketplace_rescans s WHERE s.release_id = r.id
                              AND (s.state = 'submitted' OR s.submitted_at > ?1))
          ORDER BY r.id LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![now - interval, BATCH as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Finding ids the release already answered for: every finding of its reviews' stages, and every
/// finding an earlier flag raised.
pub(crate) fn known_in(c: &Connection, release: &str) -> rusqlite::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let mut stmt = c.prepare(
        "SELECT d.evidence FROM review_decisions d JOIN review_cases c ON c.id = d.case_id
          WHERE c.release_id = ?1 AND d.evidence IS NOT NULL",
    )?;
    let evidence = stmt
        .query_map([release], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = c.prepare("SELECT findings FROM release_rescan_flags WHERE release_id = ?1")?;
    let flagged = stmt
        .query_map([release], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for text in evidence {
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        for f in v["findings"].as_array().into_iter().flatten() {
            if let Some(id) = f["id"].as_str() {
                out.insert(id.to_string());
            }
        }
    }
    for text in flagged {
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        for f in v.as_array().into_iter().flatten() {
            if let Some(id) = f["id"].as_str() {
                out.insert(id.to_string());
            }
        }
    }
    Ok(out)
}

/// The blocking findings of a re-scan that the release has not answered for before.
pub(crate) fn new_findings(result: &Value, known: &BTreeSet<String>) -> Vec<Value> {
    result["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["blocking"] == true)
        .filter(|f| f["id"].as_str().is_some_and(|id| !known.contains(id)))
        .cloned()
        .collect()
}

/// Record a finished re-scan; flag the release when it found something new. Never touches the
/// release itself.
pub(crate) fn record_in(
    c: &Connection,
    scan: &str,
    release: &str,
    result: Option<&Value>,
    error: Option<&str>,
    now: i64,
) -> rusqlite::Result<usize> {
    let Some(result) = result else {
        c.execute(
            "UPDATE marketplace_rescans SET state = 'failed', error = ?2, finished_at = ?3
              WHERE scan_id = ?1",
            params![scan, error.unwrap_or("scan failed"), now],
        )?;
        return Ok(0);
    };
    let fresh = new_findings(result, &known_in(c, release)?);
    if !fresh.is_empty() {
        c.execute(
            "INSERT INTO release_rescan_flags (id, release_id, scan_id, findings, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                new_id("rsf"),
                release,
                scan,
                Value::Array(fresh.clone()).to_string(),
                now
            ],
        )?;
        tracing::warn!(
            release,
            scan,
            new = fresh.len(),
            "re-scan flagged a release for staff"
        );
    }
    c.execute(
        "UPDATE marketplace_rescans SET state = 'done', new_findings = ?2, finished_at = ?3
          WHERE scan_id = ?1",
        params![scan, fresh.len() as i64, now],
    )?;
    Ok(fresh.len())
}

/// One pass: read finished scans, then (when a sweep is due) submit the next releases.
pub(crate) async fn work_once(state: &AppState) -> usize {
    let interval = interval_secs();
    if interval == 0 || matches!(state.nests.marketplace.buildd, Buildd::Disabled) {
        return 0;
    }
    let now = now_unix() as i64;
    let mut n = 0;
    let open: Vec<(String, String)> = state
        .store
        .group_read(|c| -> GResult<Vec<(String, String)>> {
            let mut stmt = c.prepare(
                "SELECT scan_id, release_id FROM marketplace_rescans WHERE state = 'submitted'
                  ORDER BY submitted_at LIMIT 20",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for (scan, release) in open {
        let Ok((200, v)) = state
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
        let (result, error) = match v["state"].as_str() {
            Some("succeeded") => (Some(v["result"].clone()), None),
            Some("failed") => (None, v["error"].as_str().map(str::to_string)),
            _ => continue,
        };
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<usize> {
                Ok(record_in(
                    tx,
                    &scan,
                    &release,
                    result.as_ref(),
                    error.as_deref(),
                    now,
                )?)
            })
            .await;
        n += 1;
    }
    let due = state
        .store
        .group_read(move |c| -> GResult<Vec<(String, String, String)>> {
            Ok(due_in(c, now, interval)?)
        })
        .await
        .unwrap_or_default();
    for (release, digest, sbom) in due {
        let scan = new_id("scn");
        let body = json!({ "scan_id": scan, "kind": "release", "release_id": release,
                           "image_digest": digest, "sbom": sbom });
        match state
            .nests
            .marketplace
            .buildd_call(reqwest::Method::POST, "/v1/scans", Some(body))
            .await
        {
            Ok((200..=299, _)) => {
                let _ = state
                    .store
                    .group_tx(move |tx| -> GResult<()> {
                        tx.execute(
                            "INSERT INTO marketplace_rescans (scan_id, release_id, image_digest,
                                state, submitted_at) VALUES (?1, ?2, ?3, 'submitted', ?4)",
                            params![scan, release, digest, now],
                        )?;
                        Ok(())
                    })
                    .await;
                n += 1;
            }
            other => {
                tracing::warn!(release, ?other, "re-scan not accepted by the build plane");
                break;
            }
        }
    }
    n
}

/// `GET /v1/staff/marketplace/rescan-flags` — open flags (all with `?all=true`), newest first.
async fn list_flags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        staff_gate(&state, &headers, Action::NestReview, "rescan-flags", None).await?;
        let all = q.get("all").is_some_and(|v| v == "true");
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT f.id, f.release_id, r.publisher_id, r.version, f.scan_id, f.findings,
                            f.created_at, f.acknowledged_at, f.acknowledged_by, f.note,
                            r.security_state
                       FROM release_rescan_flags f JOIN nest_releases r ON r.id = f.release_id
                      WHERE ?1 OR f.acknowledged_at IS NULL
                      ORDER BY f.created_at DESC LIMIT 100",
                )?;
                let rows = stmt
                    .query_map([all], |r| {
                        Ok(json!({
                            "flag_id": r.get::<_, String>(0)?,
                            "release_id": r.get::<_, String>(1)?,
                            "publisher_id": r.get::<_, String>(2)?,
                            "version": r.get::<_, String>(3)?,
                            "scan_id": r.get::<_, String>(4)?,
                            "findings": serde_json::from_str::<Value>(&r.get::<_, String>(5)?)
                                .unwrap_or(Value::Null),
                            "created_at": iso(r.get(6)?),
                            "acknowledged_at": r.get::<_, Option<i64>>(7)?.map(iso),
                            "acknowledged_by": r.get::<_, Option<String>>(8)?,
                            "note": r.get::<_, Option<String>>(9)?,
                            "security_state": r.get::<_, String>(10)?,
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

/// `POST /v1/staff/marketplace/rescan-flags/{flag}/acknowledge` — `{note}`: staff have looked.
/// The release is not changed; quarantine and revocation are their own routes.
async fn acknowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(flag): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (_, who) =
            staff_gate(&state, &headers, Action::NestReview, "rescan-flags", None).await?;
        let body: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
        let note = body["note"].as_str().map(|n| {
            n.chars()
                .filter(|c| !c.is_control())
                .take(1000)
                .collect::<String>()
        });
        if note.as_deref().is_none_or(|n| n.trim().is_empty()) {
            return Err(GErr::bad("invalid_request", "note: what staff decided"));
        }
        let now = now_unix() as i64;
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let open: Option<Option<i64>> = tx
                    .query_row(
                        "SELECT acknowledged_at FROM release_rescan_flags WHERE id = ?1",
                        [&flag],
                        |r| r.get(0),
                    )
                    .optional()?;
                match open {
                    None => return Err(not_found()),
                    Some(Some(_)) => {
                        return Err(crate::marketplace_m1::conflict_code(
                            "flag_acknowledged",
                            "this flag is acknowledged",
                        ))
                    }
                    Some(None) => {}
                }
                tx.execute(
                    "UPDATE release_rescan_flags SET acknowledged_at = ?2, acknowledged_by = ?3,
                        note = ?4 WHERE id = ?1",
                    params![flag, now, who, note],
                )?;
                Ok(json!({ "flag_id": flag, "acknowledged_at": iso(now) }))
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_blocking_findings_flag_a_release() {
        let known: BTreeSet<String> = ["osv-scanner:GHSA-old:x@1".to_string()].into();
        let result = json!({ "findings": [
            { "id": "osv-scanner:GHSA-old:x@1", "blocking": true },
            { "id": "grype:CVE-2026-1:openssl@3", "blocking": true },
            { "id": "grype:CVE-2026-2:zlib@1", "blocking": false },
            { "id": "yara:MAL_X:image", "blocking": true },
        ]});
        let found = new_findings(&result, &known);
        let fresh: Vec<&str> = found.iter().map(|f| f["id"].as_str().unwrap()).collect();
        assert_eq!(
            fresh,
            vec!["grype:CVE-2026-1:openssl@3", "yara:MAL_X:image"]
        );
        assert!(new_findings(&json!({}), &known).is_empty());
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE nest_releases (id TEXT PRIMARY KEY, publisher_id TEXT, version TEXT,
                image_digest TEXT, status TEXT, security_state TEXT, build_isolation TEXT, sbom TEXT);
             CREATE TABLE nest_listings (id TEXT PRIMARY KEY, release_id TEXT, status TEXT);
             CREATE TABLE review_cases (id TEXT PRIMARY KEY, release_id TEXT);
             CREATE TABLE review_decisions (id TEXT PRIMARY KEY, case_id TEXT, evidence TEXT);
             INSERT INTO nest_releases VALUES
               ('rel_live', 'pub_a', '1.0.0', 'sha256:aa', 'approved', 'normal', NULL, '{}'),
               ('rel_hosted', 'pub_a', '1.0.0', 'sha256:bb', 'approved', 'normal',
                'publisher_hosted', '{}'),
               ('rel_revoked', 'pub_a', '1.0.0', 'sha256:cc', 'approved', 'revoked', NULL, '{}'),
               ('rel_draft', 'pub_a', '1.0.0', 'sha256:dd', 'approved', 'normal', NULL, '{}');
             INSERT INTO nest_listings VALUES ('lst_1', 'rel_live', 'published'),
               ('lst_2', 'rel_hosted', 'published'), ('lst_3', 'rel_revoked', 'published'),
               ('lst_4', 'rel_draft', 'draft');
             INSERT INTO review_cases VALUES ('rvw_1', 'rel_live');",
        )
        .unwrap();
        c.execute(
            "INSERT INTO review_decisions VALUES ('dec_1', 'rvw_1', ?1)",
            [
                json!({ "findings": [{ "id": "grype:CVE-2025-1:zlib@1", "blocking": false }] })
                    .to_string(),
            ],
        )
        .unwrap();
        c.execute_batch(crate::store::marketplace_hosted::SCHEMA)
            .unwrap();
        c
    }

    /// The sweep picks published, built, unrevoked releases once per interval; a new blocking
    /// finding flags the release once, and the release and its listing are never touched.
    #[test]
    fn a_rescan_flags_a_published_release_for_staff_and_never_removes_it() {
        let c = db();
        let now = 1_800_000_000;
        let due = due_in(&c, now, 30 * 86_400).unwrap();
        assert_eq!(
            due.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(),
            ["rel_live"]
        );
        c.execute(
            "INSERT INTO marketplace_rescans (scan_id, release_id, image_digest, state,
                submitted_at) VALUES ('scn_1', 'rel_live', 'sha256:aa', 'submitted', ?1)",
            [now],
        )
        .unwrap();
        assert!(
            due_in(&c, now, 30 * 86_400).unwrap().is_empty(),
            "one at a time"
        );
        let result = json!({ "findings": [
            { "id": "grype:CVE-2025-1:zlib@1", "blocking": true },
            { "id": "grype:CVE-2026-9:openssl@3", "blocking": true, "severity": "critical" },
        ]});
        assert_eq!(
            record_in(&c, "scn_1", "rel_live", Some(&result), None, now).unwrap(),
            1
        );
        let flagged: String = c
            .query_row(
                "SELECT findings FROM release_rescan_flags WHERE release_id = 'rel_live'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            flagged.contains("CVE-2026-9") && !flagged.contains("CVE-2025-1"),
            "{flagged}"
        );
        // The same finding next month is not flagged again.
        c.execute(
            "INSERT INTO marketplace_rescans (scan_id, release_id, image_digest, state,
                submitted_at) VALUES ('scn_2', 'rel_live', 'sha256:aa', 'submitted', ?1)",
            [now + 31 * 86_400],
        )
        .unwrap();
        assert_eq!(
            record_in(&c, "scn_2", "rel_live", Some(&result), None, now).unwrap(),
            0
        );
        let (state, security, listing): (String, String, String) = c
            .query_row(
                "SELECT r.status, r.security_state, l.status FROM nest_releases r
                   JOIN nest_listings l ON l.release_id = r.id WHERE r.id = 'rel_live'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (state.as_str(), security.as_str(), listing.as_str()),
            ("approved", "normal", "published")
        );
        // A failed scan is recorded as such and flags nothing.
        c.execute(
            "INSERT INTO marketplace_rescans (scan_id, release_id, image_digest, state,
                submitted_at) VALUES ('scn_3', 'rel_live', 'sha256:aa', 'submitted', ?1)",
            [now],
        )
        .unwrap();
        assert_eq!(
            record_in(&c, "scn_3", "rel_live", None, Some("grype failed"), now).unwrap(),
            0
        );
        let st: String = c
            .query_row(
                "SELECT state FROM marketplace_rescans WHERE scan_id = 'scn_3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(st, "failed");
    }
}
