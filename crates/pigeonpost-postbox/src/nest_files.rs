//! Files on a Nest, postbox side (Nests Phase B4, R-72).
//!
//! | Route | Action | What |
//! |---|---|---|
//! | `GET /v1/nests/{nest}/files` | `NestView` | the Nest's files: name, size, kind, parse status, coverage |
//! | `POST /v1/nests/{nest}/files` | `NestOwn` | upload one file (raw body, ≤ 25 MiB, name in `x-file-name`) |
//! | `DELETE /v1/nests/{nest}/files/{file}` | `NestOwn` | remove it and everything parsed from it |
//!
//! The postbox keeps metadata only (`nest_files`); the bytes go straight to nestd over the private
//! link, which quarantines them and parses them in a separate no-network sandbox. A file's name is
//! a display string: it never becomes a path anywhere. Uploading is a person's action, bounded per
//! account per hour and by the Nest's storage quota (its package's per-Nest share).

use crate::groups::{GErr, GResult};
use crate::nest_catalog;
use crate::nests::NestdError;
use crate::policy::Action;
use crate::store::nest_funding;
use crate::{now_unix, rand_hex, sha256, AppState};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use rusqlite::params;
use serde_json::{json, Value};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nest_files (
    id             TEXT PRIMARY KEY,
    nest_id        TEXT NOT NULL,
    account_id     TEXT NOT NULL,
    name           TEXT NOT NULL,
    bytes          INTEGER NOT NULL,
    sha256         TEXT NOT NULL,
    kind           TEXT,
    status         TEXT NOT NULL CHECK (status IN ('uploading','parsed','partial','rejected','failed')),
    reason         TEXT,
    units_total    INTEGER,
    units_text     INTEGER,
    chars          INTEGER,
    warnings       TEXT,
    created_at     INTEGER NOT NULL,
    deleted_at     INTEGER
);
CREATE INDEX IF NOT EXISTS nest_files_by_nest ON nest_files(nest_id, deleted_at);
CREATE INDEX IF NOT EXISTS nest_files_by_account ON nest_files(account_id, created_at);
";

/// Largest upload (the parser's own input limit).
pub(crate) const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
/// Uploads one account may start per hour.
const UPLOADS_PER_HOUR: i64 = 60;
/// Files one Nest may hold.
const MAX_FILES: i64 = 1000;
/// Quota of a Nest that no package funds (Phase A ops Nests): the default volume.
const OPS_QUOTA_BYTES: i64 = 5 * 1024 * 1024 * 1024;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/nests/{nest}/files",
            get(list)
                .post(upload)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1)),
        )
        .route("/v1/nests/{nest}/files/{file}", delete(remove))
}

/// The caller (any role on the Nest) and the Nest's holding account and funding.
async fn owner(
    state: &AppState,
    headers: &HeaderMap,
    action: Action,
    nest: &str,
) -> GResult<(String, String, String)> {
    let (caller, _role, nref) = crate::nest_access::authorize_nest(
        state,
        headers,
        action,
        nest,
        crate::nest_access::Role::Operator,
    )
    .await?;
    Ok((caller, nref.account_id, nref.funding))
}

fn file_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let warnings: Option<String> = r.get(10)?;
    Ok(json!({
        "file_id": r.get::<_, String>(0)?,
        "name": r.get::<_, String>(1)?,
        "bytes": r.get::<_, i64>(2)?,
        "sha256": r.get::<_, String>(3)?,
        "kind": r.get::<_, Option<String>>(4)?,
        "status": r.get::<_, String>(5)?,
        "reason": r.get::<_, Option<String>>(6)?,
        "coverage": { "units_total": r.get::<_, Option<i64>>(7)?, "units_text": r.get::<_, Option<i64>>(8)? },
        "chars": r.get::<_, Option<i64>>(9)?,
        "warnings": warnings.and_then(|w| serde_json::from_str::<Value>(&w).ok()).unwrap_or(json!([])),
        "created_at": r.get::<_, i64>(11)?,
    }))
}

const FILE_COLUMNS: &str =
    "id, name, bytes, sha256, kind, status, reason, units_total, units_text, chars, warnings, created_at";

/// The Nest's storage quota in bytes: its package's per-Nest share, or the default volume.
fn quota_in(c: &rusqlite::Connection, account: &str, funding: &str, now: u64) -> GResult<i64> {
    if funding != "package" {
        return Ok(OPS_QUOTA_BYTES);
    }
    Ok(nest_funding::allowance(c, account, now)?
        .funding
        .and_then(|f| nest_catalog::by_code(f.package))
        .map_or(0, |p| p.per_nest_storage_bytes()))
}

/// `GET /v1/nests/{nest}/files`
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        let (_caller, account, funding) = owner(&state, &headers, Action::NestView, &nest).await?;
        let now = now_unix();
        let body = state
            .store
            .group_read(move |c| -> GResult<Value> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {FILE_COLUMNS} FROM nest_files WHERE nest_id = ?1 AND deleted_at IS NULL ORDER BY created_at DESC, id"
                ))?;
                let files = stmt
                    .query_map([&nest], file_json)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let used: i64 = c.query_row(
                    "SELECT COALESCE(SUM(bytes),0) FROM nest_files WHERE nest_id = ?1 AND deleted_at IS NULL",
                    [&nest],
                    |r| r.get(0),
                )?;
                Ok(json!({ "files": files, "used_bytes": used, "quota_bytes": quota_in(c, &account, &funding, now)? }))
            })
            .await?;
        Ok::<_, GErr>(Json(body).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// `POST /v1/nests/{nest}/files` — raw bytes, name in `x-file-name` (percent-encoded UTF-8).
async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
    body: Bytes,
) -> Response {
    let result = async {
        let (account, holder, funding) = owner(&state, &headers, Action::NestOwn, &nest).await?;
        if body.is_empty() || body.len() > MAX_UPLOAD_BYTES {
            return Err(GErr::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "a file of 1 byte to 25 MiB"));
        }
        let name = headers
            .get("x-file-name")
            .and_then(|v| v.to_str().ok())
            .map(percent_decode)
            .map(|n| n.chars().filter(|c| !c.is_control()).take(200).collect::<String>())
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| GErr::bad("bad_name", "x-file-name is required"))?;
        let now = now_unix();
        let file_id = format!("nfl_{}", rand_hex(12));
        let digest = crate::hex_str(&sha256(&body));
        let bytes = body.len() as i64;
        // Reserve: rate, count and quota in one transaction with the row that holds the space.
        {
            let (n, acct, fid, nm, dg) = (nest.clone(), account.clone(), file_id.clone(), name.clone(), digest.clone());
            let (funding, holder) = (funding.clone(), holder.clone());
            state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    let recent: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM nest_files WHERE account_id = ?1 AND created_at > ?2",
                        params![acct, now as i64 - 3600],
                        |r| r.get(0),
                    )?;
                    if recent >= UPLOADS_PER_HOUR {
                        return Err(GErr::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many uploads this hour"));
                    }
                    let (count, used): (i64, i64) = tx.query_row(
                        "SELECT COUNT(*), COALESCE(SUM(bytes),0) FROM nest_files WHERE nest_id = ?1 AND deleted_at IS NULL",
                        [&n],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    if count >= MAX_FILES {
                        return Err(GErr::conflict("too_many_files", "this Nest holds its maximum number of files"));
                    }
                    let quota = quota_in(tx, &holder, &funding, now)?;
                    if used + bytes > quota {
                        return Err(GErr::new(StatusCode::INSUFFICIENT_STORAGE, "storage_full", "this Nest's storage is full")
                            .with("used_bytes", used)
                            .with("quota_bytes", quota));
                    }
                    tx.execute(
                        "INSERT INTO nest_files (id, nest_id, account_id, name, bytes, sha256, status, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'uploading', ?7)",
                        params![fid, n, acct, nm, bytes, dg, now as i64],
                    )?;
                    Ok(())
                })
                .await?;
        }
        let answer = state.nests.put_file(&nest, &file_id, &name, body.to_vec()).await;
        let (status, v) = match answer {
            Ok((s, v)) if (200..300).contains(&s) => (
                match v["status"].as_str() {
                    Some("parsed") => "parsed",
                    Some("partial") => "partial",
                    _ => "rejected",
                },
                v,
            ),
            Ok((507, _)) => ("failed", json!({ "reason": "this Nest's storage is full" })),
            Ok((s, _)) => ("failed", json!({ "reason": format!("the Nests host answered {s}") })),
            Err(NestdError::Unavailable) => ("failed", json!({ "reason": "no Nests host is configured" })),
            Err(NestdError::Failed(_)) => ("failed", json!({ "reason": "the Nests host did not answer" })),
        };
        let fid = file_id.clone();
        let row = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                tx.execute(
                    "UPDATE nest_files SET status = ?2, kind = ?3, reason = ?4, units_total = ?5, units_text = ?6,
                            chars = ?7, warnings = ?8, deleted_at = CASE WHEN ?2 = 'failed' THEN ?9 ELSE NULL END
                      WHERE id = ?1",
                    params![
                        fid,
                        status,
                        v["kind"].as_str(),
                        v["reason"].as_str().map(|r| r.chars().take(300).collect::<String>()),
                        v["coverage"]["units_total"].as_i64(),
                        v["coverage"]["units_text"].as_i64(),
                        v["chars"].as_i64(),
                        v["warnings"].as_array().map(|w| Value::Array(w.iter().take(50).cloned().collect()).to_string()),
                        now_unix() as i64,
                    ],
                )?;
                Ok(tx.query_row(&format!("SELECT {FILE_COLUMNS} FROM nest_files WHERE id = ?1"), [&fid], file_json)?)
            })
            .await?;
        if status == "failed" {
            return Err(GErr::new(StatusCode::BAD_GATEWAY, "upload_failed", "the file was not stored; try again")
                .with("file", row));
        }
        Ok((StatusCode::CREATED, Json(row)).into_response())
    }
    .await;
    result.unwrap_or_else(|e: GErr| e.into_response())
}

/// `DELETE /v1/nests/{nest}/files/{file}` — removed on the Nests host first, then here.
async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((nest, file)): Path<(String, String)>,
) -> Response {
    let result = async {
        owner(&state, &headers, Action::NestOwn, &nest).await?;
        let missing = || GErr::new(StatusCode::NOT_FOUND, "no_file", "no such file");
        if !(file.starts_with("nfl_") && file.len() == 28 && file[4..].bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err(missing());
        }
        let (n, f) = (nest.clone(), file.clone());
        let exists: bool = state
            .store
            .group_read(move |c| -> GResult<bool> {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM nest_files WHERE id = ?1 AND nest_id = ?2 AND deleted_at IS NULL)",
                    params![f, n],
                    |r| r.get(0),
                )?)
            })
            .await?;
        if !exists {
            return Err(missing());
        }
        match state
            .nests
            .call(reqwest::Method::DELETE, &format!("/v1/nests/{nest}/files/{file}"), None)
            .await
        {
            Ok((s, _)) if (200..300).contains(&s) || s == 404 => {}
            _ => return Err(GErr::new(StatusCode::BAD_GATEWAY, "nestd_failed", "the Nests host did not remove it; try again")),
        }
        let f = file.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute("UPDATE nest_files SET deleted_at = ?2 WHERE id = ?1", params![f, now_unix() as i64])?;
                Ok(())
            })
            .await?;
        Ok(Json(json!({ "file_id": file, "removed": true })).into_response())
    }
    .await;
    result.unwrap_or_else(|e: GErr| e.into_response())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
