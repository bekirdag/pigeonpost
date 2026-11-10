//! What a marketplace worker may ask the broker for besides the model (the developer kit's SDKs
//! and `pigeonpost nest run`'s emulator use exactly these):
//!
//! | Route | Body | What |
//! |---|---|---|
//! | `POST /v1/progress` | `{job_id, fraction, message}` | progress of one of the worker's running jobs; the postbox turns it into `nest.job.progress` |
//! | `POST /v1/effects` (`Idempotency-Key` = `key`) | `{job_id, key, kind, summary, body, requires_approval}` | one external effect, recorded once per key |
//! | `GET /v1/secrets/{name}` | — | one of the installation's secret configuration values (`writeOnly` in `config_schema`) |
//!
//! All three authenticate the worker by its per-start broker token, exactly as the model routes
//! do, and only ever see that worker's own jobs.
//!
//! **Effects.** The first request under a key is the one recorded; the same key again answers
//! with that record (`deduplicated`), another kind under the key is `409 idempotency_conflict`.
//! A release whose manifest does not declare `external_writes` gets `403 effect_refused` (the
//! marketplace pool offers none today, so in practice every effect is refused and recorded as
//! such). One that does and asks for approval waits (`202 awaiting_approval`) until the postbox
//! approves or rejects it; an approved effect is handed to the platform's executor
//! (`approved`), which reports `done` with a reference or `outcome_unknown`. An approved effect
//! whose worker stops before an outcome is reported becomes `outcome_unknown`: never retried
//! blindly, reconciled by the postbox (the job goes to `reconciling` and is delivered again with
//! the same id, so the app asks under the same key).
//!
//! **Secrets (W1.6).** The postbox sends an installation's secrets with each job over the private
//! control link. They are sealed here (XChaCha20-Poly1305) under a key derived from the worker's
//! current broker token, so only a request presenting that token can open them: a new start (new
//! token) needs them sent again, a stopped worker's are deleted with its token, and the ledger
//! holds ciphertext only. They are never in the job file, the environment or a log.
//!
//! Limits: 100 progress updates and 16 effects per job, a 200-character progress message, a
//! 500-character effect summary, a 64 KiB effect body, 32 secrets of at most 4 KiB.

use crate::db::Db;
use crate::util::{now, sha256_hex};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const MAX_PROGRESS_UPDATES: i64 = 100;
pub const MAX_PROGRESS_MESSAGE: usize = 200;
pub const MAX_EFFECTS_PER_JOB: i64 = 16;
pub const MAX_EFFECT_SUMMARY: usize = 500;
pub const MAX_EFFECT_BODY: usize = 64 * 1024;
pub const MAX_SECRETS: usize = 32;
pub const MAX_SECRET_BYTES: usize = 4096;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mp_job_progress (
    job_id   TEXT PRIMARY KEY,
    fraction REAL NOT NULL,
    message  TEXT NOT NULL,
    updates  INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
-- Whether a job's release declares external writes (from the postbox, with the job).
CREATE TABLE IF NOT EXISTS mp_job_writes (
    job_id TEXT PRIMARY KEY,
    allowed INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mp_effects (
    nest_id   TEXT NOT NULL,
    key       TEXT NOT NULL,
    job_id    TEXT NOT NULL,
    kind      TEXT NOT NULL,
    summary   TEXT NOT NULL,
    body      TEXT NOT NULL,
    requires_approval INTEGER NOT NULL,
    state     TEXT NOT NULL CHECK (state IN
        ('refused','awaiting_approval','rejected','approved','done','outcome_unknown')),
    reference TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (nest_id, key)
);
CREATE INDEX IF NOT EXISTS mp_effects_by_job ON mp_effects(job_id);
-- Installation secrets, sealed under the worker's current broker token (never the token).
CREATE TABLE IF NOT EXISTS mp_secrets (
    nest_id      TEXT PRIMARY KEY,
    token_sha256 TEXT NOT NULL,
    nonce        BLOB NOT NULL,
    sealed       BLOB NOT NULL,
    updated_at   INTEGER NOT NULL
);
";

/// An error a worker route answers with: `(status, code, detail)`.
pub type Refusal = (u16, &'static str, String);

fn refuse(status: u16, code: &'static str, detail: impl Into<String>) -> Refusal {
    (status, code, detail.into())
}

fn sql(e: impl std::fmt::Display) -> Refusal {
    refuse(500, "internal", e.to_string())
}

/// The worker a broker token belongs to, or `401`.
pub fn worker_of(db: &Db, token: &str) -> Result<String, Refusal> {
    db.nest_for_token(&sha256_hex(token.as_bytes()))
        .map_err(sql)?
        .map(|n| n.id)
        .ok_or_else(|| refuse(401, "unauthorized", "unknown or expired token"))
}

/// A running job of `nest`, or `404` (another worker's job is not this worker's business).
fn own_running_job(db: &Db, nest: &str, job: &str) -> Result<(), Refusal> {
    match db.mp_job(job).map_err(sql)? {
        Some((n, state, _)) if n == nest && state == "running" => Ok(()),
        _ => Err(refuse(
            404,
            "unknown_job",
            "no running job of this worker has that id",
        )),
    }
}

/// `POST /v1/progress`.
pub fn progress(db: &Db, token: &str, body: &Value) -> Result<Value, Refusal> {
    let nest = worker_of(db, token)?;
    let job = body["job_id"].as_str().unwrap_or_default();
    own_running_job(db, &nest, job)?;
    let fraction = body["fraction"]
        .as_f64()
        .filter(|f| f.is_finite())
        .ok_or_else(|| refuse(400, "bad_request", "fraction: a number from 0 to 1"))?
        .clamp(0.0, 1.0);
    let message: String = body["message"]
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PROGRESS_MESSAGE)
        .collect();
    let updates = db
        .raw(|c| {
            c.query_row(
                "SELECT updates FROM mp_job_progress WHERE job_id = ?1",
                params![job],
                |r| r.get::<_, i64>(0),
            )
            .optional()
        })
        .map_err(sql)?
        .unwrap_or(0);
    if updates >= MAX_PROGRESS_UPDATES {
        return Err(refuse(
            429,
            "too_many_updates",
            format!("at most {MAX_PROGRESS_UPDATES} progress updates per job"),
        ));
    }
    db.raw(|c| {
        c.execute(
            "INSERT INTO mp_job_progress (job_id, fraction, message, updates, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(job_id) DO UPDATE SET fraction = ?2, message = ?3,
                 updates = updates + 1, updated_at = ?4",
            params![job, fraction, message, now()],
        )
    })
    .map_err(sql)?;
    Ok(json!({ "ok": true }))
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn kind_ok(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind.split('.').all(|w| {
            w.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// `(job_id, kind, summary, body, state, reference)` of a recorded effect.
type EffectRow = (String, String, String, String, String, Option<String>);

fn effect_in(db: &Db, nest: &str, key: &str) -> Result<Option<EffectRow>, Refusal> {
    db.raw(|c| {
        c.query_row(
            "SELECT job_id, kind, summary, body, state, reference FROM mp_effects
              WHERE nest_id = ?1 AND key = ?2",
            params![nest, key],
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
    })
    .map_err(sql)
}

/// The answer for a recorded effect, by its state: `(status, body)`.
fn effect_answer(key: &str, row: &EffectRow, deduplicated: bool) -> (u16, Value) {
    let (_, _, _, body, state, reference) = row;
    match state.as_str() {
        "done" => (
            200,
            json!({
                "state": "done", "deduplicated": deduplicated,
                "outcome": { "reference": reference,
                             "body": serde_json::from_str::<Value>(body).unwrap_or(Value::Null) },
            }),
        ),
        "awaiting_approval" | "approved" => (202, json!({ "state": state, "key": key })),
        "refused" => (
            403,
            json!({ "error": "effect_refused",
                    "detail": "this release declares no external writes" }),
        ),
        "rejected" => (
            403,
            json!({ "error": "effect_refused", "detail": "the buyer rejected this effect" }),
        ),
        _ => (
            409,
            json!({ "error": "effect_outcome_unknown",
                    "detail": "the outcome is being reconciled; it is never retried blindly" }),
        ),
    }
}

/// `POST /v1/effects`: `(status, body)`.
pub fn effect(
    db: &Db,
    token: &str,
    idempotency_key: &str,
    body: &Value,
) -> Result<(u16, Value), Refusal> {
    let nest = worker_of(db, token)?;
    let key = body["key"].as_str().unwrap_or_default();
    let kind = body["kind"].as_str().unwrap_or_default();
    if !key_ok(key) || idempotency_key != key {
        return Err(refuse(
            400,
            "bad_request",
            "an effect needs a key, sent as the Idempotency-Key header too",
        ));
    }
    if !kind_ok(kind) {
        return Err(refuse(400, "bad_request", "kind: dotted lower-case words"));
    }
    let summary = body["summary"].as_str().unwrap_or("");
    if summary.chars().count() > MAX_EFFECT_SUMMARY {
        return Err(refuse(
            400,
            "bad_request",
            "summary: at most 500 characters",
        ));
    }
    let payload = body.get("body").cloned().unwrap_or(json!({}));
    let text = payload.to_string();
    if text.len() > MAX_EFFECT_BODY {
        return Err(refuse(413, "too_large", "an effect body is at most 64 KiB"));
    }
    if let Some(row) = effect_in(db, &nest, key)? {
        if row.1 != kind {
            return Err(refuse(
                409,
                "idempotency_conflict",
                "this key was used for another kind of effect",
            ));
        }
        return Ok(effect_answer(key, &row, true));
    }
    let job = body["job_id"].as_str().unwrap_or_default();
    own_running_job(db, &nest, job)?;
    let count: i64 = db
        .raw(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM mp_effects WHERE job_id = ?1",
                params![job],
                |r| r.get(0),
            )
        })
        .map_err(sql)?;
    if count >= MAX_EFFECTS_PER_JOB {
        return Err(refuse(
            429,
            "too_many_effects",
            format!("at most {MAX_EFFECTS_PER_JOB} effects per job"),
        ));
    }
    let allowed = db
        .raw(|c| {
            c.query_row(
                "SELECT allowed FROM mp_job_writes WHERE job_id = ?1",
                params![job],
                |r| r.get::<_, bool>(0),
            )
            .optional()
        })
        .map_err(sql)?
        .unwrap_or(false);
    let requires_approval = body["requires_approval"] == true;
    let state = if !allowed {
        "refused"
    } else if requires_approval {
        "awaiting_approval"
    } else {
        "approved"
    };
    let t = now();
    db.raw(|c| {
        c.execute(
            "INSERT INTO mp_effects (nest_id, key, job_id, kind, summary, body, requires_approval,
                state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                nest,
                key,
                job,
                kind,
                summary,
                text,
                requires_approval,
                state,
                t
            ],
        )
    })
    .map_err(sql)?;
    let row = (
        job.to_string(),
        kind.to_string(),
        summary.to_string(),
        text,
        state.to_string(),
        None,
    );
    Ok(effect_answer(key, &row, false))
}

/// The postbox's side of an effect (control API): `approve`, `reject`, `done` (with a
/// reference) or `unknown`. Only a forward move is taken; a finished effect never changes.
pub fn settle_effect(
    db: &Db,
    job: &str,
    key: &str,
    action: &str,
    reference: Option<&str>,
) -> Result<Value, Refusal> {
    let (from, to): (&[&str], &str) = match action {
        "approve" => (&["awaiting_approval"], "approved"),
        "reject" => (&["awaiting_approval"], "rejected"),
        "done" => (&["approved", "outcome_unknown"], "done"),
        "unknown" => (&["approved"], "outcome_unknown"),
        _ => {
            return Err(refuse(
                400,
                "bad_request",
                "action: approve, reject, done or unknown",
            ))
        }
    };
    if action == "done" && !reference.is_some_and(|r| !r.is_empty() && r.len() <= 200) {
        return Err(refuse(400, "bad_request", "done needs a reference"));
    }
    let n = db
        .raw(|c| {
            let states = from
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(",");
            c.execute(
                &format!(
                    "UPDATE mp_effects SET state = ?3, reference = COALESCE(?4, reference),
                        updated_at = ?5 WHERE job_id = ?1 AND key = ?2 AND state IN ({states})"
                ),
                params![job, key, to, reference, now()],
            )
        })
        .map_err(sql)?;
    if n == 0 {
        return Err(refuse(
            409,
            "effect_state",
            "no effect in a state that takes this",
        ));
    }
    Ok(json!({ "job_id": job, "key": key, "state": to }))
}

/// What the postbox reads with a job's state: its latest progress and its effects.
pub fn job_extras(db: &Db, job: &str) -> Value {
    let progress = db
        .raw(|c| {
            c.query_row(
                "SELECT fraction, message, updates FROM mp_job_progress WHERE job_id = ?1",
                params![job],
                |r| {
                    Ok(json!({
                        "fraction": r.get::<_, f64>(0)?,
                        "message": r.get::<_, String>(1)?,
                        "updates": r.get::<_, i64>(2)?,
                    }))
                },
            )
            .optional()
        })
        .ok()
        .flatten();
    let effects = db
        .raw(|c| {
            let mut stmt = c.prepare(
                "SELECT key, kind, summary, state, reference FROM mp_effects WHERE job_id = ?1
                  ORDER BY created_at, key",
            )?;
            let rows = stmt.query_map(params![job], |r| {
                Ok(json!({
                    "key": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "summary": r.get::<_, String>(2)?,
                    "state": r.get::<_, String>(3)?,
                    "reference": r.get::<_, Option<String>>(4)?,
                }))
            })?;
            rows.collect::<rusqlite::Result<Vec<Value>>>()
        })
        .unwrap_or_default();
    json!({ "progress": progress, "effects": effects })
}

/// A worker stopped: effects it had approved and not finished have an unknown outcome.
pub fn worker_stopped(db: &Db, nest: &str) {
    let _ = db.raw(|c| {
        c.execute(
            "UPDATE mp_effects SET state = 'outcome_unknown', updated_at = ?2
              WHERE nest_id = ?1 AND state = 'approved'",
            params![nest, now()],
        )?;
        c.execute("DELETE FROM mp_secrets WHERE nest_id = ?1", params![nest])
    });
}

fn secret_key(token: &str) -> chacha20poly1305::Key {
    let mut h = Sha256::new();
    h.update(b"pigeonpost-nest-secrets-v1\0");
    h.update(token.as_bytes());
    chacha20poly1305::Key::clone_from_slice(&h.finalize())
}

/// Seal `secrets` for the worker's current start (`token`, read from its volume by the control
/// service). An empty set removes what was stored.
pub fn store_secrets(
    db: &Db,
    nest: &str,
    token: &str,
    secrets: &Map<String, Value>,
) -> Result<(), String> {
    if secrets.is_empty() {
        return db
            .raw(|c| c.execute("DELETE FROM mp_secrets WHERE nest_id = ?1", params![nest]))
            .map(|_| ());
    }
    if secrets.len() > MAX_SECRETS
        || secrets.iter().any(|(k, v)| {
            !pigeonpost_core::nest_package::field_name_ok(k)
                || v.as_str().is_none_or(|s| s.len() > MAX_SECRET_BYTES)
        })
    {
        return Err("secrets: at most 32 named strings of at most 4 KiB".into());
    }
    let plain = zeroize::Zeroizing::new(Value::Object(secrets.clone()).to_string());
    let nonce_bytes: [u8; 24] = {
        let mut n = [0u8; 24];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut n);
        n
    };
    let cipher = XChaCha20Poly1305::new(&secret_key(token));
    let token_sha = sha256_hex(token.as_bytes());
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: plain.as_bytes(),
                aad: format!("{nest}\0{token_sha}").as_bytes(),
            },
        )
        .map_err(|_| "secrets: sealing failed".to_string())?;
    db.raw(|c| {
        c.execute(
            "INSERT INTO mp_secrets (nest_id, token_sha256, nonce, sealed, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(nest_id) DO UPDATE SET token_sha256 = ?2, nonce = ?3, sealed = ?4,
                 updated_at = ?5",
            params![nest, token_sha, nonce_bytes.to_vec(), sealed, now()],
        )
    })
    .map(|_| ())
}

/// `GET /v1/secrets/{name}`: one secret of the worker presenting `token`, for its current start.
pub fn secret(db: &Db, token: &str, name: &str) -> Result<Value, Refusal> {
    let nest = worker_of(db, token)?;
    if !pigeonpost_core::nest_package::field_name_ok(name) {
        return Err(refuse(400, "bad_request", "a secret name: [a-z][a-z0-9_]*"));
    }
    let token_sha = sha256_hex(token.as_bytes());
    let row: Option<(String, Vec<u8>, Vec<u8>)> = db
        .raw(|c| {
            c.query_row(
                "SELECT token_sha256, nonce, sealed FROM mp_secrets WHERE nest_id = ?1",
                params![nest],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
        .map_err(sql)?;
    let not_set = || {
        refuse(
            404,
            "secret_not_set",
            "this installation has no such secret",
        )
    };
    let Some((sealed_for, nonce, sealed)) = row else {
        return Err(not_set());
    };
    if sealed_for != token_sha || nonce.len() != 24 {
        // Sealed for an earlier start: the next job brings them again.
        return Err(not_set());
    }
    let cipher = XChaCha20Poly1305::new(&secret_key(token));
    let plain = zeroize::Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &sealed,
                    aad: format!("{nest}\0{token_sha}").as_bytes(),
                },
            )
            .map_err(|_| not_set())?,
    );
    let all: Value = serde_json::from_slice(&plain).map_err(|_| not_set())?;
    match all.get(name).and_then(Value::as_str) {
        Some(v) => Ok(json!({ "name": name, "value": v })),
        None => Err(not_set()),
    }
}
