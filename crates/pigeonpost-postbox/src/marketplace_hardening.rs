//! Marketplace runtime hardening (W1 in `docs/planning/2026-11-10-marketplace-production-readiness.md`):
//! what the postbox needs before untrusted publisher code may run.
//!
//! - **Signed attestations** (W1.1): every attestation statement names the key it is signed with
//!   (`key_id`) and carries an Ed25519 signature over `SIGNATURE_CONTEXT || statement`. The
//!   signing key is a 32-byte seed in hex in a root-only file on the postbox host
//!   (`POSTBOX_MARKETPLACE_ATTESTATION_KEY_FILE`), never in the repository or the database.
//!   Admission here and on nestd (at admission and at every dispatch) verifies it; nestd trusts
//!   only the public keys in its own configuration. Rotation: see `pigeonpost_nests::attest`.
//! - **Events** (W1.7): `nest.job.*`, `nest.review.*` and `nest.security.*` typed events on
//!   `/v1/events`, ids and states only, written in the same transaction as the change.
//! - **Abuse controls** (W1.8): per-publisher quotas (source builds per day, concurrent
//!   workers, monthly broker tokens; the last two are enforced by nestd from the limits sent
//!   with each job), a job rate limit per buyer, and a kill switch per listing and per publisher.
//! - **Remote build plane** (W1.3/W1.5): source builds from a git repository and commit, run by
//!   `pigeonpost-nest-buildd` on another host; the postbox submits and pulls the result, so the
//!   build host holds no postbox credential.
//! - **Worker tombstones** (W1.2): a destroyed buyer worker's id is kept and replayed to nestd
//!   on unlock with the first-party tombstones.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `POST /v1/staff/marketplace/listings/{listing}/kill` | `NestQuarantine` | staff with step-up |
//! | `POST /v1/staff/marketplace/publishers/{publisher}/kill` | `NestQuarantine` | staff with step-up |
//! | `PUT /v1/staff/marketplace/publishers/{publisher}/quotas` | `NestQuarantine` | staff with step-up |
//! | `POST /v1/staff/marketplace/source-builds` | `NestReview` | staff with step-up |
//! | `GET /v1/staff/marketplace/source-builds/{build}` | `NestReview` | staff with step-up |

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, ip_of, not_found};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::marketplace as store;
use crate::{now_unix, AppState, ClientIp};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// Domain separator of an attestation signature (`pigeonpost_nests::attest::SIGNATURE_CONTEXT`).
pub(crate) const SIGNATURE_CONTEXT: &str = "pigeonpost-nest-attestation-sig-v1\n";

pub(crate) const SCHEMA: &str = "
-- W1.1: the signature over each attestation statement, append-only like the attestation.
CREATE TABLE IF NOT EXISTS release_attestation_signatures (
    attestation_id TEXT PRIMARY KEY REFERENCES release_attestations(id),
    key_id         TEXT NOT NULL,
    signature      TEXT NOT NULL,
    created_at     INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS release_attestation_signatures_no_update
BEFORE UPDATE ON release_attestation_signatures
BEGIN SELECT RAISE(ABORT, 'release_attestation_signatures are append-only'); END;
CREATE TRIGGER IF NOT EXISTS release_attestation_signatures_no_delete
BEFORE DELETE ON release_attestation_signatures
BEGIN SELECT RAISE(ABORT, 'release_attestation_signatures are append-only'); END;

-- W1.8: per-publisher quotas staff set; a publisher without a row has the configured defaults.
CREATE TABLE IF NOT EXISTS marketplace_publisher_quotas (
    publisher_id          TEXT PRIMARY KEY REFERENCES publisher_profiles(id),
    builds_per_day        INTEGER NOT NULL CHECK (builds_per_day >= 0),
    max_workers           INTEGER NOT NULL CHECK (max_workers >= 0),
    monthly_broker_tokens INTEGER NOT NULL CHECK (monthly_broker_tokens >= 0),
    jobs_per_minute       INTEGER NOT NULL CHECK (jobs_per_minute >= 0),
    updated_by            TEXT NOT NULL,
    updated_at            INTEGER NOT NULL
);

-- W1.8: the kill switch. `marketplace_kills` is the append-only log; `marketplace_kill_state` is
-- what is in force; `marketplace_runtime_stops` is what nestd must be told, until it says yes.
CREATE TABLE IF NOT EXISTS marketplace_kills (
    id          TEXT PRIMARY KEY,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('listing','publisher')),
    target_id   TEXT NOT NULL,
    action      TEXT NOT NULL CHECK (action IN ('kill','restore')),
    reason      TEXT NOT NULL,
    actor       TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS marketplace_kills_no_update BEFORE UPDATE ON marketplace_kills
BEGIN SELECT RAISE(ABORT, 'marketplace_kills are append-only'); END;
CREATE TABLE IF NOT EXISTS marketplace_kill_state (
    target_kind TEXT NOT NULL CHECK (target_kind IN ('listing','publisher')),
    target_id   TEXT NOT NULL,
    killed      INTEGER NOT NULL CHECK (killed IN (0, 1)),
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (target_kind, target_id)
);
CREATE TABLE IF NOT EXISTS marketplace_runtime_stops (
    listing_id TEXT PRIMARY KEY,
    stop       INTEGER NOT NULL CHECK (stop IN (0, 1)),
    acked      INTEGER NOT NULL DEFAULT 0 CHECK (acked IN (0, 1)),
    updated_at INTEGER NOT NULL
);

-- W1.3/W1.5: source builds on the remote build plane.
CREATE TABLE IF NOT EXISTS marketplace_source_builds (
    id           TEXT PRIMARY KEY,
    publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),
    repo_url     TEXT NOT NULL,
    commit_sha   TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('requested','submitted','registered','failed')),
    requested_by TEXT NOT NULL,
    release_id   TEXT,
    review_id    TEXT,
    error        TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS marketplace_source_builds_by_publisher
    ON marketplace_source_builds(publisher_id, created_at);
CREATE INDEX IF NOT EXISTS marketplace_source_builds_open
    ON marketplace_source_builds(state);

-- W1.2: destroyed buyer workers; replayed to nestd as tombstones on unlock.
CREATE TABLE IF NOT EXISTS marketplace_worker_tombstones (
    nest_id    TEXT PRIMARY KEY,
    listing_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
";

// ---- configuration --------------------------------------------------------------------------------

/// Defaults for a publisher without its own quota row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Quotas {
    pub builds_per_day: i64,
    pub max_workers: i64,
    pub monthly_broker_tokens: i64,
    pub jobs_per_minute: i64,
}

impl Default for Quotas {
    fn default() -> Self {
        Quotas {
            builds_per_day: 5,
            max_workers: 20,
            monthly_broker_tokens: 2_000_000,
            jobs_per_minute: 10,
        }
    }
}

/// One call the postbox made (or, in tests, would have made) to the build plane.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuilddCall {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
}

/// Where the build plane is.
pub(crate) enum Buildd {
    Disabled,
    Http {
        url: String,
        token: zeroize::Zeroizing<String>,
        client: reqwest::Client,
    },
    /// Tests: record calls and answer from a queue (`200 {}` when it is empty).
    #[cfg_attr(not(test), allow(dead_code))]
    Capture {
        calls: Mutex<Vec<BuilddCall>>,
        answers: Mutex<std::collections::VecDeque<(u16, Value)>>,
    },
}

pub(crate) struct Hardening {
    /// The attestation signing key and its id. `None`: no approval can be attested.
    signer: Option<(SigningKey, String)>,
    /// Public keys whose attestations are still accepted here (the signer's own included).
    trusted: Vec<VerifyingKey>,
    pub(crate) quotas: Quotas,
    pub(crate) buildd: Buildd,
    /// The newest typed event already woken for (see [`wake_new_events`]).
    woken_through: AtomicI64,
}

impl Default for Hardening {
    fn default() -> Self {
        Hardening {
            signer: None,
            trusted: Vec::new(),
            quotas: Quotas::default(),
            buildd: Buildd::Disabled,
            woken_through: AtomicI64::new(0),
        }
    }
}

/// `ppk_` and the first 16 hex digits of SHA-256 over the public key.
pub(crate) fn key_id(key: &VerifyingKey) -> String {
    let h: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("ppk_{}", &h[..16])
}

fn unhex<const N: usize>(t: &str) -> Option<[u8; N]> {
    let t = t.trim();
    if t.len() != 2 * N || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&t[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn env_i64(var: &str, default: i64) -> i64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(default)
}

/// A secret file's trimmed content; refuses a file others can read.
fn read_secret(path: &str) -> Option<zeroize::Zeroizing<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).ok()?.permissions().mode();
        if mode & 0o037 != 0 {
            tracing::warn!(%path, "secret file is readable by others; ignored");
            return None;
        }
    }
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(path).ok()?);
    let value = text.trim();
    (!value.is_empty()).then(|| zeroize::Zeroizing::new(value.to_string()))
}

impl Hardening {
    pub(crate) fn from_env() -> Self {
        let mut h = Hardening::default();
        if let Ok(path) = std::env::var("POSTBOX_MARKETPLACE_ATTESTATION_KEY_FILE") {
            match read_secret(path.trim()).and_then(|s| unhex::<32>(&s)) {
                Some(seed) => {
                    let key = SigningKey::from_bytes(&seed);
                    let id = key_id(&key.verifying_key());
                    tracing::info!(key_id = %id, "marketplace attestation key loaded");
                    h.trusted.push(key.verifying_key());
                    h.signer = Some((key, id));
                }
                None => tracing::warn!(
                    "POSTBOX_MARKETPLACE_ATTESTATION_KEY_FILE does not hold a 64-hex seed; \
                     marketplace approvals cannot be attested"
                ),
            }
        }
        // Rotation: the previous key(s) stay trusted for attestations made with them.
        for k in std::env::var("POSTBOX_MARKETPLACE_ATTESTATION_TRUSTED_KEYS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            match unhex::<32>(k).and_then(|b| VerifyingKey::from_bytes(&b).ok()) {
                Some(v) if !v.is_weak() => {
                    if !h.trusted.contains(&v) {
                        h.trusted.push(v);
                    }
                }
                _ => tracing::warn!(
                    "a POSTBOX_MARKETPLACE_ATTESTATION_TRUSTED_KEYS entry is unusable"
                ),
            }
        }
        let d = Quotas::default();
        h.quotas = Quotas {
            builds_per_day: env_i64("POSTBOX_MARKETPLACE_BUILDS_PER_DAY", d.builds_per_day),
            max_workers: env_i64("POSTBOX_MARKETPLACE_MAX_WORKERS", d.max_workers),
            monthly_broker_tokens: env_i64(
                "POSTBOX_MARKETPLACE_MONTHLY_BROKER_TOKENS",
                d.monthly_broker_tokens,
            ),
            jobs_per_minute: env_i64("POSTBOX_MARKETPLACE_JOBS_PER_MINUTE", d.jobs_per_minute),
        };
        let url = std::env::var("POSTBOX_MARKETPLACE_BUILDD_URL")
            .unwrap_or_default()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let token = std::env::var("POSTBOX_MARKETPLACE_BUILDD_TOKEN_FILE")
            .ok()
            .and_then(|p| read_secret(p.trim()));
        if !url.is_empty() {
            match (crate::nests::url_ok(&url), token) {
                (true, Some(token)) if token.len() >= 32 => {
                    if let Ok(client) = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(20))
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                    {
                        tracing::info!("marketplace build plane configured");
                        h.buildd = Buildd::Http { url, token, client };
                    }
                }
                _ => tracing::warn!(
                    "POSTBOX_MARKETPLACE_BUILDD_URL/_TOKEN_FILE unusable; source builds disabled"
                ),
            }
        }
        h
    }

    /// Tests: a fixed signing key, the same as `pigeonpost_nests::attest::test_key`, and a
    /// recording build plane.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let id = key_id(&key.verifying_key());
        Hardening {
            trusted: vec![key.verifying_key()],
            signer: Some((key, id)),
            buildd: Buildd::Capture {
                calls: Mutex::new(Vec::new()),
                answers: Mutex::new(std::collections::VecDeque::new()),
            },
            ..Hardening::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn buildd_calls(&self) -> Vec<BuilddCall> {
        match &self.buildd {
            Buildd::Capture { calls, .. } => calls.lock().expect("capture").clone(),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn buildd_answer(&self, status: u16, body: Value) {
        if let Buildd::Capture { answers, .. } = &self.buildd {
            answers.lock().expect("capture").push_back((status, body));
        }
    }

    /// The signing key and its id, or `503 attestation_key_missing`.
    pub(crate) fn signer(&self) -> GResult<(SigningKey, String)> {
        self.signer.clone().ok_or_else(|| {
            GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "attestation_key_missing",
                "this postbox has no attestation key; approvals cannot be attested",
            )
        })
    }

    /// Does `signature` (hex) over `statement` verify under the trusted key the statement names?
    pub(crate) fn verify(&self, statement: &str, key_id_: &str, signature: &str) -> bool {
        let named = serde_json::from_str::<Value>(statement)
            .ok()
            .and_then(|s| s["key_id"].as_str().map(str::to_string));
        if named.as_deref() != Some(key_id_) {
            return false;
        }
        let Some(key) = self.trusted.iter().find(|k| key_id(k) == key_id_) else {
            return false;
        };
        let Some(sig) = unhex::<64>(signature) else {
            return false;
        };
        key.verify_strict(&message(statement), &Signature::from_bytes(&sig))
            .is_ok()
    }

    async fn buildd_call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        match &self.buildd {
            Buildd::Disabled => Err("the build plane is not configured".into()),
            Buildd::Capture { calls, answers } => {
                calls.lock().expect("capture").push(BuilddCall {
                    method: method.to_string(),
                    path: path.to_string(),
                    body,
                });
                Ok(answers
                    .lock()
                    .expect("capture")
                    .pop_front()
                    .unwrap_or((200, json!({}))))
            }
            Buildd::Http { url, token, client } => {
                let mut req = client
                    .request(method, format!("{url}{path}"))
                    .bearer_auth(token.as_str());
                if let Some(b) = body {
                    req = req.json(&b);
                }
                let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
                let status = resp.status().as_u16();
                Ok((status, resp.json::<Value>().await.unwrap_or(Value::Null)))
            }
        }
    }
}

fn message(statement: &str) -> Vec<u8> {
    let mut m = SIGNATURE_CONTEXT.as_bytes().to_vec();
    m.extend_from_slice(statement.as_bytes());
    m
}

/// Sign a statement: 128 hex digits.
pub(crate) fn sign(key: &SigningKey, statement: &str) -> String {
    hex(&key.sign(&message(statement)).to_bytes())
}

/// Keep the signature next to its attestation.
pub(crate) fn record_signature_in(
    tx: &Transaction<'_>,
    attestation_id: &str,
    key_id_: &str,
    signature: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO release_attestation_signatures (attestation_id, key_id, signature, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![attestation_id, key_id_, signature, now],
    )
    .map(|_| ())
}

/// `(key_id, signature)` of the release's newest attestation; `None` when that one is unsigned.
pub(crate) fn signature_in(
    c: &Connection,
    release: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let row: Option<(Option<String>, Option<String>)> = c
        .query_row(
            "SELECT s.key_id, s.signature FROM release_attestations a
               LEFT JOIN release_attestation_signatures s ON s.attestation_id = a.id
              WHERE a.release_id = ?1 AND a.statement IS NOT NULL
              ORDER BY a.created_at DESC, a.id LIMIT 1",
            [release],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(k, s)| Some((k?, s?))))
}

// ---- events ---------------------------------------------------------------------------------------

/// The account a publisher's events go to: its owning account, its organization's owner, or the
/// platform owner for Pigeonpost's own publisher.
fn publisher_account_in(
    c: &Connection,
    publisher: &str,
    platform_owner: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT owner_kind, owner_id FROM publisher_profiles WHERE id = ?1",
            [publisher],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((kind, id)) if kind == "account" => Some(id),
        Some((kind, id)) if kind == "org" => c
            .query_row("SELECT owner_account FROM orgs WHERE id = ?1", [&id], |r| {
                r.get(0)
            })
            .optional()?,
        Some(_) => platform_owner.map(str::to_string),
        None => None,
    })
}

fn append(c: &Connection, account: &str, kind: &str, data: &Value, now: i64) -> GResult<()> {
    crate::store::append_event(c, account, kind, None, data, now.max(0) as u64)?;
    Ok(())
}

/// The typed event for one job event: `nest.job.admitted|progress|result_ready|reconciling` to
/// the account that submitted the job. A job stopped for security is `nest.job.progress` with
/// its new execution state.
pub(crate) fn job_event_in(
    c: &Connection,
    job: &str,
    kind: &str,
    detail: &Value,
    now: i64,
) -> GResult<()> {
    let typed = match kind {
        "nest.job.admitted"
        | "nest.job.progress"
        | "nest.job.result_ready"
        | "nest.job.reconciling" => kind,
        _ => "nest.job.progress",
    };
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT account_id, listing_id, release_id FROM nest_jobs WHERE id = ?1",
            [job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((account, listing, release)) = row {
        append(
            c,
            &account,
            typed,
            &json!({
                "job_id": job,
                "listing_id": listing,
                "release_id": release,
                "execution": detail["execution"],
            }),
            now,
        )?;
    }
    Ok(())
}

/// `nest.review.stage_changed|blocked|decision|completed` to the publisher of the review.
pub(crate) fn review_event_in(
    c: &Connection,
    review: &str,
    kind: &str,
    extra: Value,
    platform_owner: Option<&str>,
    now: i64,
) -> GResult<()> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT release_id, publisher_id FROM review_cases WHERE id = ?1",
            [review],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((release, publisher)) = row else {
        return Ok(());
    };
    let Some(account) = publisher_account_in(c, &publisher, platform_owner)? else {
        return Ok(());
    };
    let mut data = json!({
        "review_id": review,
        "release_id": release,
        "publisher_id": publisher,
    });
    if let (Some(d), Value::Object(e)) = (data.as_object_mut(), extra) {
        d.extend(e);
    }
    append(c, &account, &format!("nest.review.{kind}"), &data, now)
}

/// `nest.security.quarantined|revoked` to the publisher and every account with a job on the
/// release (or, for a kill, on the listing).
pub(crate) fn security_event_in(
    c: &Connection,
    kind: &str,
    release: Option<&str>,
    listing: Option<&str>,
    platform_owner: Option<&str>,
    now: i64,
) -> GResult<()> {
    let mut accounts: Vec<String> = Vec::new();
    let mut publisher: Option<String> = None;
    if let Some(r) = release {
        let mut stmt =
            c.prepare("SELECT DISTINCT account_id FROM nest_jobs WHERE release_id = ?1")?;
        accounts.extend(
            stmt.query_map([r], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        publisher = c
            .query_row(
                "SELECT publisher_id FROM nest_releases WHERE id = ?1",
                [r],
                |row| row.get(0),
            )
            .optional()?;
    }
    if let Some(l) = listing {
        let mut stmt =
            c.prepare("SELECT DISTINCT account_id FROM nest_jobs WHERE listing_id = ?1")?;
        accounts.extend(
            stmt.query_map([l], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        if publisher.is_none() {
            publisher = c
                .query_row(
                    "SELECT publisher_id FROM nest_listings WHERE id = ?1",
                    [l],
                    |row| row.get(0),
                )
                .optional()?;
        }
    }
    if let Some(p) = &publisher {
        if let Some(a) = publisher_account_in(c, p, platform_owner)? {
            accounts.push(a);
        }
    }
    accounts.sort();
    accounts.dedup();
    let data = json!({ "release_id": release, "listing_id": listing, "publisher_id": publisher });
    for a in accounts {
        append(c, &a, &format!("nest.security.{kind}"), &data, now)?;
    }
    Ok(())
}

/// Wake the open event streams of every account a marketplace event was written for since the
/// last call. The dispatcher loop calls it each tick and the staff routes after they commit, so
/// a typed event reaches an open stream within one tick.
pub(crate) async fn wake_new_events(state: &AppState) {
    let since = state.nests.marketplace.woken_through.load(Ordering::SeqCst);
    let found = state
        .store
        .group_read(move |c| -> GResult<(i64, Vec<String>)> {
            let max: i64 =
                c.query_row("SELECT COALESCE(MAX(id), 0) FROM account_events", [], |r| {
                    r.get(0)
                })?;
            let mut stmt = c.prepare(
                "SELECT DISTINCT account_id FROM account_events
                  WHERE id > ?1 AND id <= ?2 AND type LIKE 'nest.%'",
            )?;
            let rows = stmt
                .query_map(params![since, max], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((max, rows))
        })
        .await;
    if let Ok((max, accounts)) = found {
        for a in accounts {
            state.event_hub.wake(&a);
        }
        state
            .nests
            .marketplace
            .woken_through
            .fetch_max(max, Ordering::SeqCst);
    }
}

// ---- abuse controls ---------------------------------------------------------------------------------

fn quotas_in(c: &Connection, publisher: &str, d: Quotas) -> rusqlite::Result<Quotas> {
    Ok(c.query_row(
        "SELECT builds_per_day, max_workers, monthly_broker_tokens, jobs_per_minute
           FROM marketplace_publisher_quotas WHERE publisher_id = ?1",
        [publisher],
        |r| {
            Ok(Quotas {
                builds_per_day: r.get(0)?,
                max_workers: r.get(1)?,
                monthly_broker_tokens: r.get(2)?,
                jobs_per_minute: r.get(3)?,
            })
        },
    )
    .optional()?
    .unwrap_or(d))
}

/// The limits nestd enforces for a job on `release`; `None` for the platform's own releases.
pub(crate) fn limits_in(
    c: &Connection,
    release: &str,
    d: Quotas,
) -> rusqlite::Result<Option<Value>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT p.id, p.owner_kind FROM nest_releases r
               JOIN publisher_profiles p ON p.id = r.publisher_id WHERE r.id = ?1",
            [release],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((publisher, kind)) = row else {
        return Ok(None);
    };
    if kind == "platform" {
        return Ok(None);
    }
    let q = quotas_in(c, &publisher, d)?;
    Ok(Some(json!({
        "publisher_id": publisher,
        "max_workers": q.max_workers.clamp(0, i64::from(u32::MAX)),
        "monthly_tokens": q.monthly_broker_tokens,
    })))
}

/// Is the listing, or its publisher, killed?
pub(crate) fn killed_in(c: &Connection, listing: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM marketplace_kill_state k
                  WHERE k.killed = 1 AND (
                        (k.target_kind = 'listing' AND k.target_id = ?1)
                     OR (k.target_kind = 'publisher' AND k.target_id =
                            (SELECT publisher_id FROM nest_listings WHERE id = ?1))))",
        [listing],
        |r| r.get(0),
    )
}

/// The per-buyer job rate limit: at most the listing publisher's `jobs_per_minute` jobs from one
/// account in any minute (`429 rate_limited`).
pub(crate) async fn rate_limit_jobs(state: &AppState, account: &str, address: &str) -> GResult<()> {
    let (who, to, d) = (
        account.to_string(),
        address.to_string(),
        state.nests.marketplace.quotas,
    );
    let now = now_unix() as i64;
    let (count, limit) = state
        .store
        .group_read(move |c| -> GResult<(i64, i64)> {
            let publisher: Option<String> = c
                .query_row(
                    "SELECT publisher_id FROM nest_listings WHERE address = ?1",
                    [&to],
                    |r| r.get(0),
                )
                .optional()?;
            let limit = match publisher {
                Some(p) => quotas_in(c, &p, d)?.jobs_per_minute,
                None => d.jobs_per_minute,
            };
            let count: i64 = c.query_row(
                "SELECT COUNT(*) FROM nest_jobs WHERE account_id = ?1 AND created_at > ?2",
                params![who, now - 60],
                |r| r.get(0),
            )?;
            Ok((count, limit))
        })
        .await?;
    if count >= limit {
        let mut e = GErr::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many jobs in the last minute; try again shortly",
        );
        e.retry_after = Some(60);
        return Err(e);
    }
    Ok(())
}

/// Tell nestd about every listing stop or restore it has not acknowledged. The kill route calls
/// it at once and the dispatcher loop every tick, so a worker host that was unreachable stops
/// the workers as soon as it answers again.
pub(crate) async fn reconcile_runtime_stops(state: &AppState) -> usize {
    let pending = state
        .store
        .group_read(|c| -> GResult<Vec<(String, bool)>> {
            let mut stmt = c.prepare(
                "SELECT listing_id, stop FROM marketplace_runtime_stops WHERE acked = 0
                  ORDER BY updated_at LIMIT 100",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? == 1))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let mut acked = 0;
    for (listing, stop) in pending {
        let r = state
            .nests
            .call(
                reqwest::Method::POST,
                &format!("/v1/mp/listings/{listing}/stop"),
                Some(json!({ "stop": stop })),
            )
            .await;
        if matches!(r, Ok((200, _))) {
            let l = listing.clone();
            let ok = state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute(
                        "UPDATE marketplace_runtime_stops SET acked = 1
                          WHERE listing_id = ?1 AND stop = ?2",
                        params![l, stop as i64],
                    )?;
                    Ok(())
                })
                .await;
            if ok.is_ok() {
                acked += 1;
            }
        }
    }
    acked
}

// ---- worker tombstones ------------------------------------------------------------------------------

/// Destroy one buyer's worker of a listing on the worker host (uninstall), keeping its id as a
/// tombstone. Returns the destroyed worker id, if there was one. For M2's uninstall path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn destroy_worker(
    state: &AppState,
    listing: &str,
    buyer_context: &str,
) -> GResult<Option<String>> {
    let r = state
        .nests
        .call(
            reqwest::Method::POST,
            "/v1/mp/workers/destroy",
            Some(json!({ "listing_id": listing, "buyer_context": buyer_context })),
        )
        .await;
    let destroyed = match r {
        Ok((200, v)) => v["destroyed"].as_str().map(str::to_string),
        _ => {
            return Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "nests_unavailable",
                "the worker host did not destroy the worker; try again",
            ))
        }
    };
    if let Some(id) = destroyed.clone() {
        let (l, now) = (listing.to_string(), now_unix() as i64);
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "INSERT INTO marketplace_worker_tombstones (nest_id, listing_id, created_at)
                     VALUES (?1, ?2, ?3) ON CONFLICT(nest_id) DO NOTHING",
                    params![id, l, now],
                )?;
                Ok(())
            })
            .await?;
    }
    Ok(destroyed)
}

/// The SQL the unlocker's tombstone list unions in (`nests::tombstones`).
pub(crate) const WORKER_TOMBSTONES_SQL: &str =
    "SELECT nest_id FROM marketplace_worker_tombstones ORDER BY created_at DESC LIMIT 1000";

// ---- staff routes -----------------------------------------------------------------------------------

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/staff/marketplace/listings/{listing}/kill",
            post(kill_listing),
        )
        .route(
            "/v1/staff/marketplace/publishers/{publisher}/kill",
            post(kill_publisher),
        )
        .route(
            "/v1/staff/marketplace/publishers/{publisher}/quotas",
            put(set_quotas),
        )
        .route("/v1/staff/marketplace/source-builds", post(request_build))
        .route(
            "/v1/staff/marketplace/source-builds/{build}",
            get(read_build),
        )
}

/// Staff role and step-up for `action` on `subject`.
async fn staff_only(
    state: &AppState,
    principal: &crate::Principal,
    account: &str,
    action: Action,
    subject: &str,
) -> GResult<()> {
    let who = account.to_string();
    let roles = state
        .store
        .group_read(move |c| -> GResult<_> { Ok(store::live_staff_roles_in(c, &who)?) })
        .await?;
    let f = Facts {
        staff_roles: roles,
        ..facts(state)
    };
    authorize(
        &Actor::Principal(principal),
        action,
        &Resource::StaffSubject(subject),
        &f,
    )
    .map_err(|d| deny(d, false))
}

fn reason_ok(reason: &str) -> GResult<String> {
    let r = reason.trim().to_string();
    if r.is_empty() || r.chars().count() > 2000 {
        return Err(GErr::bad("invalid_request", "reason: 1-2000 characters"));
    }
    Ok(r)
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KillReq {
    /// `kill` or `restore`.
    action: String,
    reason: String,
}

async fn kill_listing(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(listing): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    into_response(kill(state, client_ip, headers, "listing", listing, body).await)
}

async fn kill_publisher(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    into_response(kill(state, client_ip, headers, "publisher", publisher, body).await)
}

/// The kill switch. In one transaction: the log row, the state in force, every queued or running
/// job of the affected listings stopped for security, the runtime stops nestd must apply, the
/// audit row and the events. Then nestd is told at once; whatever it did not acknowledge, the
/// dispatcher loop resends every tick. Workers stop within one nestd `podman rm --time 10` of
/// nestd being reachable.
async fn kill(
    state: AppState,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    kind: &'static str,
    target: String,
    body: Result<Json<Value>, JsonRejection>,
) -> GResult<(StatusCode, Json<Value>)> {
    let (principal, account) = caller(&state, &headers).await?;
    let req: KillReq = crate::marketplace::parse(body)?;
    let killing = match req.action.as_str() {
        "kill" => true,
        "restore" => false,
        _ => return Err(GErr::bad("invalid_request", "action: kill or restore")),
    };
    let reason = reason_ok(&req.reason)?;
    staff_only(
        &state,
        &principal,
        &account,
        Action::NestQuarantine,
        &target,
    )
    .await?;
    let now = now_unix() as i64;
    let audit = state.auditor.row(
        crate::audit::ActorKind::of(&principal),
        Some(account.clone()),
        &format!("marketplace.{kind}.{}", req.action),
        Some(format!("{kind}:{target}")),
        json!({ "action": req.action }),
        ip_of(&client_ip),
        user_agent(&headers),
        now as u64,
    );
    let owner = state.platform_owner.as_deref().map(str::to_string);
    let (t, who) = (target.clone(), account.clone());
    let out = state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let listings: Vec<String> = if kind == "listing" {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM nest_listings WHERE id = ?1)",
                    [&t],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(not_found());
                }
                vec![t.clone()]
            } else {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM publisher_profiles WHERE id = ?1)",
                    [&t],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(not_found());
                }
                let mut stmt = tx.prepare("SELECT id FROM nest_listings WHERE publisher_id = ?1")?;
                let rows = stmt
                    .query_map([&t], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            tx.execute(
                "INSERT INTO marketplace_kills (id, target_kind, target_id, action, reason, actor, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    format!("kil_{}", crate::groups::rand_b32(16)),
                    kind,
                    t,
                    if killing { "kill" } else { "restore" },
                    reason,
                    who,
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO marketplace_kill_state (target_kind, target_id, killed, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(target_kind, target_id) DO UPDATE SET killed = excluded.killed,
                    updated_at = excluded.updated_at",
                params![kind, t, killing as i64, now],
            )?;
            let mut stopped = 0i64;
            for l in &listings {
                // A restore of the publisher leaves a listing killed on its own stopped.
                let stop = killing || killed_in(tx, l)?;
                tx.execute(
                    "INSERT INTO marketplace_runtime_stops (listing_id, stop, acked, updated_at)
                     VALUES (?1, ?2, 0, ?3)
                     ON CONFLICT(listing_id) DO UPDATE SET stop = excluded.stop, acked = 0,
                        updated_at = excluded.updated_at",
                    params![l, stop as i64, now],
                )?;
                if !killing {
                    continue;
                }
                let jobs: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM nest_jobs WHERE listing_id = ?1
                           AND execution IN ('queued','running','waiting_input','reconciling')",
                    )?;
                    let rows = stmt
                        .query_map([l], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                };
                for job in jobs {
                    tx.execute(
                        "UPDATE nest_jobs SET execution = 'stopped_security', error_code = 'listing_killed',
                            revision = revision + 1, updated_at = ?2 WHERE id = ?1",
                        params![job, now],
                    )?;
                    let detail = json!({ "execution": "stopped_security" });
                    tx.execute(
                        "INSERT INTO nest_job_events (job_id, kind, detail, created_at)
                         VALUES (?1, 'nest.security.stopped', ?2, ?3)",
                        params![job, detail.to_string(), now],
                    )?;
                    job_event_in(tx, &job, "nest.security.stopped", &detail, now)?;
                    stopped += 1;
                }
                if killing {
                    security_event_in(tx, "quarantined", None, Some(l), owner.as_deref(), now)?;
                }
            }
            crate::store::audit::append_in(tx, &audit)?;
            Ok(json!({
                "target_kind": kind,
                "target_id": t,
                "killed": killing,
                "listings": listings,
                "jobs_stopped": stopped,
            }))
        })
        .await?;
    let acked = reconcile_runtime_stops(&state).await;
    wake_new_events(&state).await;
    let mut out = out;
    let total = out["listings"].as_array().map(Vec::len).unwrap_or(0);
    out["runtime_updated"] = json!(acked >= total);
    Ok((StatusCode::OK, Json(json!({ "data": out }))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaReq {
    builds_per_day: i64,
    max_workers: i64,
    monthly_broker_tokens: i64,
    jobs_per_minute: i64,
    reason: String,
}

/// `PUT /v1/staff/marketplace/publishers/{publisher}/quotas`
async fn set_quotas(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let q: QuotaReq = crate::marketplace::parse(body)?;
        let reason = reason_ok(&q.reason)?;
        for v in [q.builds_per_day, q.max_workers, q.monthly_broker_tokens, q.jobs_per_minute] {
            if !(0..=1_000_000_000_000).contains(&v) {
                return Err(GErr::bad("invalid_request", "quotas are whole numbers from 0"));
            }
        }
        staff_only(&state, &principal, &account, Action::NestQuarantine, &publisher).await?;
        let now = now_unix() as i64;
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "marketplace.publisher.quotas",
            Some(format!("publisher:{publisher}")),
            json!({
                "builds_per_day": q.builds_per_day,
                "max_workers": q.max_workers,
                "monthly_broker_tokens": q.monthly_broker_tokens,
                "jobs_per_minute": q.jobs_per_minute,
                "reason": reason,
            }),
            ip_of(&client_ip),
            user_agent(&headers),
            now as u64,
        );
        let (p, who) = (publisher.clone(), account.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM publisher_profiles WHERE id = ?1)",
                    [&p],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(not_found());
                }
                tx.execute(
                    "INSERT INTO marketplace_publisher_quotas (publisher_id, builds_per_day,
                        max_workers, monthly_broker_tokens, jobs_per_minute, updated_by, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(publisher_id) DO UPDATE SET builds_per_day = excluded.builds_per_day,
                        max_workers = excluded.max_workers,
                        monthly_broker_tokens = excluded.monthly_broker_tokens,
                        jobs_per_minute = excluded.jobs_per_minute,
                        updated_by = excluded.updated_by, updated_at = excluded.updated_at",
                    params![
                        p,
                        q.builds_per_day,
                        q.max_workers,
                        q.monthly_broker_tokens,
                        q.jobs_per_minute,
                        who,
                        now
                    ],
                )?;
                crate::store::audit::append_in(tx, &audit)?;
                Ok(())
            })
            .await?;
        Ok((
            StatusCode::OK,
            Json(json!({ "data": {
                "publisher_id": publisher,
                "builds_per_day": q.builds_per_day,
                "max_workers": q.max_workers,
                "monthly_broker_tokens": q.monthly_broker_tokens,
                "jobs_per_minute": q.jobs_per_minute,
            }})),
        ))
    }
    .await;
    into_response(result)
}

// ---- source builds on the remote build plane -----------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceBuildReq {
    publisher_id: String,
    repo_url: String,
    commit: String,
}

fn source_ok(repo_url: &str, commit: &str) -> GResult<()> {
    let bad = |m: &str| GErr::bad("invalid_request", m.to_string());
    let full_hex = (commit.len() == 40 || commit.len() == 64)
        && commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !full_hex {
        return Err(bad("commit: a full lower-case object id"));
    }
    let ok = repo_url.strip_prefix("https://").is_some_and(|rest| {
        rest.split_once('/').is_some_and(|(host, path)| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
                && !path.is_empty()
                && !path.contains("..")
                && path.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'/' | b'-')
                })
        })
    }) && repo_url.len() <= 300;
    if !ok {
        return Err(bad("repo_url: https://<git host>/<path>"));
    }
    Ok(())
}

/// Queue a source build for an approved publisher, within its builds-per-day quota. The build
/// plane checks the git host against its own allowlist. For staff now, and for M2's publisher
/// submission route.
pub(crate) async fn request_source_build(
    state: &AppState,
    publisher_id: &str,
    repo_url: &str,
    commit: &str,
    requested_by: &str,
) -> GResult<Value> {
    source_ok(repo_url, commit)?;
    let d = state.nests.marketplace.quotas;
    let now = now_unix() as i64;
    let id = format!("sbd_{}", crate::groups::rand_b32(16));
    let (p, url, c_, who, bid) = (
        publisher_id.to_string(),
        repo_url.to_string(),
        commit.to_string(),
        requested_by.to_string(),
        id.clone(),
    );
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let status: Option<String> = tx
                .query_row(
                    "SELECT status FROM publisher_profiles WHERE id = ?1",
                    [&p],
                    |r| r.get(0),
                )
                .optional()?;
            match status.as_deref() {
                None => return Err(not_found()),
                Some("approved") => {}
                Some(_) => {
                    return Err(GErr::new(
                        StatusCode::CONFLICT,
                        "publisher_not_approved",
                        "only an approved publisher can submit builds",
                    ))
                }
            }
            let killed: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM marketplace_kill_state
                  WHERE target_kind = 'publisher' AND target_id = ?1 AND killed = 1)",
                [&p],
                |r| r.get(0),
            )?;
            if killed {
                return Err(GErr::new(
                    StatusCode::CONFLICT,
                    "publisher_suspended",
                    "this publisher is stopped by the platform",
                ));
            }
            let q = quotas_in(tx, &p, d)?;
            let today: i64 = tx.query_row(
                "SELECT COUNT(*) FROM marketplace_source_builds
                  WHERE publisher_id = ?1 AND created_at > ?2",
                params![p, now - 86_400],
                |r| r.get(0),
            )?;
            if today >= q.builds_per_day {
                return Err(GErr::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "quota_exceeded",
                    "this publisher has used its builds for the day",
                )
                .with("builds_per_day", q.builds_per_day));
            }
            tx.execute(
                "INSERT INTO marketplace_source_builds (id, publisher_id, repo_url, commit_sha, state,
                    requested_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'requested', ?5, ?6, ?6)",
                params![bid, p, url, c_, who, now],
            )?;
            Ok(())
        })
        .await?;
    source_builds_once(state).await;
    read_source_build(state, &id).await?.ok_or_else(not_found)
}

async fn read_source_build(state: &AppState, id: &str) -> GResult<Option<Value>> {
    let id = id.to_string();
    state
        .store
        .group_read(move |c| -> GResult<Option<Value>> {
            Ok(c.query_row(
                "SELECT id, publisher_id, repo_url, commit_sha, state, release_id, review_id, error
                   FROM marketplace_source_builds WHERE id = ?1",
                [&id],
                |r| {
                    Ok(json!({
                        "build_id": r.get::<_, String>(0)?,
                        "publisher_id": r.get::<_, String>(1)?,
                        "repo_url": r.get::<_, String>(2)?,
                        "commit": r.get::<_, String>(3)?,
                        "state": r.get::<_, String>(4)?,
                        "release_id": r.get::<_, Option<String>>(5)?,
                        "review_id": r.get::<_, Option<String>>(6)?,
                        "error": r.get::<_, Option<String>>(7)?,
                    }))
                },
            )
            .optional()?)
        })
        .await
}

async fn request_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let req: SourceBuildReq = crate::marketplace::parse(body)?;
        staff_only(
            &state,
            &principal,
            &account,
            Action::NestReview,
            &req.publisher_id,
        )
        .await?;
        let v = request_source_build(
            &state,
            &req.publisher_id,
            &req.repo_url,
            &req.commit,
            &account,
        )
        .await?;
        Ok((StatusCode::ACCEPTED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

async fn read_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(build): Path<String>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        staff_only(&state, &principal, &account, Action::NestReview, &build).await?;
        let v = read_source_build(&state, &build)
            .await?
            .ok_or_else(not_found)?;
        Ok((StatusCode::OK, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

async fn set_build_state(state: &AppState, id: &str, next: &str, error: Option<String>) {
    let (id, next, now) = (id.to_string(), next.to_string(), now_unix() as i64);
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "UPDATE marketplace_source_builds SET state = ?2, error = ?3, updated_at = ?4
                  WHERE id = ?1",
                params![id, next, error, now],
            )?;
            Ok(())
        })
        .await;
}

/// `(id, publisher, repo_url, commit, state)` of an unfinished source build.
type OpenBuild = (String, String, String, String, String);

/// Move source builds along: submit requested ones to the build plane, and record finished ones
/// as releases under review with their analysis stages. Returns how many changed.
pub(crate) async fn source_builds_once(state: &AppState) -> usize {
    if matches!(state.nests.marketplace.buildd, Buildd::Disabled) {
        return 0;
    }
    let open = state
        .store
        .group_read(|c| -> GResult<Vec<OpenBuild>> {
            let mut stmt = c.prepare(
                "SELECT id, publisher_id, repo_url, commit_sha, state FROM marketplace_source_builds
                  WHERE state IN ('requested','submitted') ORDER BY created_at LIMIT 20",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let h = &state.nests.marketplace;
    let mut changed = 0;
    for (id, publisher, url, commit, st) in open {
        if st == "requested" {
            let body = json!({ "build_id": id, "publisher_id": publisher, "repo_url": url, "commit": commit });
            match h
                .buildd_call(reqwest::Method::POST, "/v1/builds", Some(body))
                .await
            {
                Ok((202 | 200, _)) => {
                    set_build_state(state, &id, "submitted", None).await;
                    changed += 1;
                }
                Ok((status, v)) if (400..500).contains(&status) && status != 429 => {
                    let e = v["detail"]
                        .as_str()
                        .or(v["error"].as_str())
                        .unwrap_or("refused");
                    set_build_state(state, &id, "failed", Some(e.chars().take(500).collect()))
                        .await;
                    changed += 1;
                }
                _ => {}
            }
            continue;
        }
        let Ok((200, v)) = h
            .buildd_call(reqwest::Method::GET, &format!("/v1/builds/{id}"), None)
            .await
        else {
            continue;
        };
        match v["state"].as_str() {
            Some("failed") => {
                let e = v["error"]
                    .as_str()
                    .unwrap_or("build failed")
                    .chars()
                    .take(500)
                    .collect();
                set_build_state(state, &id, "failed", Some(e)).await;
                changed += 1;
            }
            Some("succeeded") => {
                match crate::marketplace_m1::register_source_build(state, &id, &publisher, &v).await
                {
                    Ok((release, review)) => {
                        let (bid, now) = (id.clone(), now_unix() as i64);
                        let _ = state
                            .store
                            .group_tx(move |tx| -> GResult<()> {
                                tx.execute(
                                    "UPDATE marketplace_source_builds SET state = 'registered',
                                        release_id = ?2, review_id = ?3, updated_at = ?4 WHERE id = ?1",
                                    params![bid, release, review, now],
                                )?;
                                Ok(())
                            })
                            .await;
                    }
                    Err(e) => {
                        set_build_state(
                            state,
                            &id,
                            "failed",
                            Some(format!("{}: {}", e.code, e.message)),
                        )
                        .await;
                    }
                }
                changed += 1;
            }
            _ => {}
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker host (`pigeonpost_nests::attest`) asserts the same vector.
    #[test]
    fn the_signature_vector_matches_the_worker_host() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let id = key_id(&key.verifying_key());
        assert_eq!(id, "ppk_fe812c12f3ab4ce6");
        let statement = format!("{{\"key_id\":\"{id}\",\"type\":\"t\"}}");
        assert_eq!(&sign(&key, &statement)[..16], "7873323f311d7fea");
        let h = Hardening::for_test();
        assert!(h.verify(&statement, &id, &sign(&key, &statement)));
        assert!(!h.verify(&statement.replace('t', "u"), &id, &sign(&key, &statement)));
        // A statement must name the key it is signed with.
        assert!(!h.verify("{\"type\":\"t\"}", &id, &sign(&key, "{\"type\":\"t\"}")));
        // An unknown key is not trusted.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let oid = key_id(&other.verifying_key());
        let os = format!("{{\"key_id\":\"{oid}\"}}");
        assert!(!h.verify(&os, &oid, &sign(&other, &os)));
    }

    #[test]
    fn source_urls_are_plain_https_with_full_commits() {
        let c = "0123456789abcdef0123456789abcdef01234567";
        assert!(source_ok("https://git.example.org/acme/x.git", c).is_ok());
        for (u, k) in [
            ("http://git.example.org/acme/x", c),
            ("https://git.example.org/../x", c),
            ("https://user@git.example.org/x", c),
            ("https://git.example.org/x", "main"),
        ] {
            assert!(source_ok(u, k).is_err(), "{u}");
        }
    }
}
