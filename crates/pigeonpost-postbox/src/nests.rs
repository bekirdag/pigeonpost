//! Nests Phase A: hosted agents on the worker host (docs/nests/phase-a-plan.md,
//! docs/nests/postbox-integration.md).
//!
//! # Who decides what
//!
//! The postbox stays the authority for ownership: which account a Nest belongs to, which mailbox
//! it is, whether the `nests` flag is on for that account and how many Nests it may hold. `nestd`
//! on the worker host owns the runtime (sandbox, volume, run/usage ledger). Calls go one way only,
//! postbox → nestd, over the private WireGuard link, with nestd's bearer token.
//!
//! # The mailbox credential never leaves the sandbox
//!
//! Creating a Nest mints a custodial agent mailbox under the owner's account and **discards** the
//! capability token that minting returns: nobody ever holds it. What the postbox hands to nestd is
//! a single-use enrollment code (15 minutes, stored here only as a hash). The sandbox generates
//! an Ed25519 device key, signs the code and redeems it on `POST /v1/nests/enroll`; redemption
//! rotates the mailbox's capability token and returns the new one to the sandbox alone. Neither
//! the operator, the postbox's internal callers nor nestd ever see a working credential. Deleting a
//! Nest rotates the token again to a value nobody knows, which revokes the sandbox.
//!
//! # Admission
//!
//! Phase A never auto-grants a stranger: the only senders a new Nest acts for automatically are
//! mailboxes of the same account, named at creation, and only for `answer_question`.

use crate::audit::ActorKind;
use crate::groups::{parse, rate_limit, GErr, GResult};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::{
    capabilities, gen_cap_token, hex_str, now_unix, rand_hex, sha256, store, AppState, ClientIp,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// One enrollment row joined with its Nest, as redemption reads it.
type EnrollRow = (
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Enrollment codes live fifteen minutes: long enough for a cold sandbox start, short enough that
/// a leaked code is worth little.
pub(crate) const CODE_SECS: u64 = 900;
/// The signed message a sandbox proves its device key with.
pub(crate) const ENROLL_CONTEXT: &str = "pigeonpost-nest-enroll-v1\0";
/// What a new Nest grants its owner's own mailboxes: questions as request envelopes (Phase A) and
/// plain conversation (`chat`, Phase B3).
const GRANTED_VERBS: &[&str] = &["answer_question", crate::verbs::NEST_CHAT];
const MAX_ALLOW_SENDERS: usize = 8;
/// At most one wake per Nest in this many seconds.
pub(crate) const WAKE_DEBOUNCE_SECS: u64 = 10;

// ---- nestd client -----------------------------------------------------------------------------

/// One request the postbox made (or, in tests, would have made) to nestd.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NestdCall {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
}

/// Where nestd is.
pub(crate) enum Transport {
    /// No `POSTBOX_NESTS_URL`/`POSTBOX_NESTS_TOKEN`: every lifecycle call answers `503`.
    Disabled,
    Http {
        url: String,
        token: String,
        client: reqwest::Client,
    },
    /// Tests: record each call and answer it; `fail` makes every call answer `500`.
    #[cfg_attr(not(test), allow(dead_code))]
    Capture {
        calls: Mutex<Vec<NestdCall>>,
        fail: AtomicBool,
    },
}

pub(crate) struct Runtime {
    pub(crate) transport: Transport,
    /// Non-deleted Nests one account may hold (`POSTBOX_NESTS_MAX_PER_ACCOUNT`, default 1).
    pub(crate) max_per_account: i64,
    /// Monthly model budget handed to nestd for each new Nest.
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    /// Last wake sent per Nest, for the debounce.
    woken: Mutex<HashMap<String, u64>>,
    /// Phase B6: the Nests host's key-encryption key, released to nestd over the private link
    /// (`POSTBOX_NESTS_KEK_FILE`, 64 hex characters). Never logged, never sent anywhere else.
    pub(crate) kek: Option<zeroize::Zeroizing<String>>,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime {
            transport: Transport::Disabled,
            max_per_account: 1,
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            woken: Mutex::new(HashMap::new()),
            kek: None,
        }
    }
}

/// Why a nestd call did not produce an answer.
#[derive(Debug)]
pub(crate) enum NestdError {
    Unavailable,
    Failed(String),
}

/// `https://…`, or plain `http://` only to loopback or an RFC 1918 IPv4 literal — the WireGuard
/// link to the worker host encrypts that hop; a public plain-HTTP URL is refused.
pub(crate) fn url_ok(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty();
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.split(':').next().unwrap_or("");
    match host.parse::<std::net::Ipv4Addr>() {
        Ok(ip) => ip.is_loopback() || ip.is_private(),
        Err(_) => false,
    }
}

fn env_num(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

impl Runtime {
    pub(crate) fn from_env() -> Self {
        let d = Runtime::default();
        let url = std::env::var("POSTBOX_NESTS_URL")
            .unwrap_or_default()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let token = std::env::var("POSTBOX_NESTS_TOKEN")
            .unwrap_or_default()
            .trim()
            .to_string();
        let transport = if url.is_empty() && token.is_empty() {
            Transport::Disabled
        } else if !url_ok(&url) || token.len() < 32 {
            tracing::warn!("POSTBOX_NESTS_URL/POSTBOX_NESTS_TOKEN unusable; Nests disabled");
            Transport::Disabled
        } else {
            match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(client) => {
                    tracing::info!("Nests configured (nestd on the worker host)");
                    Transport::Http { url, token, client }
                }
                Err(_) => Transport::Disabled,
            }
        };
        Runtime {
            transport,
            max_per_account: env_num("POSTBOX_NESTS_MAX_PER_ACCOUNT", 1).min(1_000) as i64,
            input_tokens: env_num("POSTBOX_NESTS_INPUT_TOKENS", d.input_tokens),
            output_tokens: env_num("POSTBOX_NESTS_OUTPUT_TOKENS", d.output_tokens),
            woken: Mutex::new(HashMap::new()),
            kek: read_kek(),
        }
    }

    /// Tests: record calls instead of sending them.
    #[cfg(test)]
    pub(crate) fn capture() -> Self {
        Runtime {
            transport: Transport::Capture {
                calls: Mutex::new(Vec::new()),
                fail: AtomicBool::new(false),
            },
            ..Runtime::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn calls(&self) -> Vec<NestdCall> {
        match &self.transport {
            Transport::Capture { calls, .. } => calls.lock().expect("capture lock").clone(),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_fail(&self, on: bool) {
        if let Transport::Capture { fail, .. } = &self.transport {
            fail.store(on, Ordering::SeqCst);
        }
    }

    pub(crate) fn configured(&self) -> bool {
        !matches!(self.transport, Transport::Disabled)
    }

    /// One call to nestd: `(status, JSON body)` for any answer, an error when there was none.
    pub(crate) async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), NestdError> {
        match &self.transport {
            Transport::Disabled => Err(NestdError::Unavailable),
            Transport::Capture { calls, fail } => {
                calls.lock().expect("capture lock").push(NestdCall {
                    method: method.to_string(),
                    path: path.to_string(),
                    body: body.clone(),
                });
                if fail.load(Ordering::SeqCst) {
                    return Ok((500, json!({ "error": "capture_failure" })));
                }
                let state = match (method.as_str(), path.rsplit('/').next()) {
                    ("POST", Some("pause")) => "paused",
                    ("POST", Some("stop")) => "stopped",
                    ("POST", _) => "running",
                    ("DELETE", _) => "deleted",
                    _ => "running",
                };
                Ok((200, json!({ "state": state })))
            }
            Transport::Http { url, token, client } => {
                let mut req = client
                    .request(method, format!("{url}{path}"))
                    .bearer_auth(token);
                if let Some(body) = body {
                    req = req.json(&body);
                }
                let resp = req
                    .send()
                    .await
                    .map_err(|e| NestdError::Failed(e.without_url().to_string()))?;
                let status = resp.status().as_u16();
                let value = resp.json::<Value>().await.unwrap_or(Value::Null);
                Ok((status, value))
            }
        }
    }
}

impl Runtime {
    /// Upload one file's bytes to nestd (`PUT /v1/nests/{id}/files/{file}`), Phase B4. Longer
    /// timeout than lifecycle calls: nestd parses the file in its own sandbox before answering.
    pub(crate) async fn put_file(
        &self,
        nest_id: &str,
        file_id: &str,
        name: &str,
        data: Vec<u8>,
    ) -> Result<(u16, Value), NestdError> {
        let path = format!("/v1/nests/{nest_id}/files/{file_id}");
        match &self.transport {
            Transport::Disabled => Err(NestdError::Unavailable),
            Transport::Capture { calls, fail } => {
                calls.lock().expect("capture lock").push(NestdCall {
                    method: "PUT".into(),
                    path,
                    body: Some(json!({ "bytes": data.len(), "name": name })),
                });
                if fail.load(Ordering::SeqCst) {
                    return Ok((500, json!({ "error": "capture_failure" })));
                }
                // The parser itself is tested in pigeonpost-nests; this answers as it would.
                let m = pigeonpost_core_parse_stub(&data);
                Ok((200, m))
            }
            Transport::Http { url, token, client } => {
                let encoded: String = name
                    .bytes()
                    .map(|b| {
                        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                            (b as char).to_string()
                        } else {
                            format!("%{b:02X}")
                        }
                    })
                    .collect();
                let resp = client
                    .put(format!("{url}{path}"))
                    .bearer_auth(token)
                    .header("content-type", "application/octet-stream")
                    .header("x-nest-file-name", encoded)
                    .timeout(std::time::Duration::from_secs(180))
                    .body(data)
                    .send()
                    .await
                    .map_err(|e| NestdError::Failed(e.without_url().to_string()))?;
                let status = resp.status().as_u16();
                let value = resp.json::<Value>().await.unwrap_or(Value::Null);
                Ok((status, value))
            }
        }
    }
}

impl Runtime {
    /// Download bytes from nestd (the export zip, Phase B5). Bounded by nestd's own export limit.
    pub(crate) async fn get_bytes(&self, path: &str) -> Result<(u16, Vec<u8>), NestdError> {
        match &self.transport {
            Transport::Disabled => Err(NestdError::Unavailable),
            Transport::Capture { calls, fail } => {
                calls.lock().expect("capture lock").push(NestdCall {
                    method: "GET".into(),
                    path: path.to_string(),
                    body: None,
                });
                if fail.load(Ordering::SeqCst) {
                    return Ok((500, Vec::new()));
                }
                // An empty zip: end-of-central-directory record only.
                let mut zip = b"PK\x05\x06".to_vec();
                zip.extend_from_slice(&[0u8; 18]);
                Ok((200, zip))
            }
            Transport::Http { url, token, client } => {
                let resp = client
                    .get(format!("{url}{path}"))
                    .bearer_auth(token)
                    .timeout(std::time::Duration::from_secs(600))
                    .send()
                    .await
                    .map_err(|e| NestdError::Failed(e.without_url().to_string()))?;
                let status = resp.status().as_u16();
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| NestdError::Failed(e.without_url().to_string()))?;
                Ok((status, bytes.to_vec()))
            }
        }
    }
}

/// Tests only: what nestd answers for an upload, without parsing (text-like bytes parse, others
/// are rejected), so postbox tests do not depend on the Nests crate.
fn pigeonpost_core_parse_stub(data: &[u8]) -> Value {
    let text = std::str::from_utf8(data).is_ok() && !data.contains(&0);
    json!({
        "kind": if text { "text" } else { "unknown" },
        "status": if text { "parsed" } else { "rejected" },
        "reason": if text { Value::Null } else { json!("unsupported file type") },
        "bytes": data.len(),
        "coverage": { "units_total": 1, "units_text": if text { 1 } else { 0 } },
        "parts": if text { 1 } else { 0 },
        "chars": if text { data.len() } else { 0 },
        "warnings": [],
    })
}

/// Is `address` the mailbox of a Nest that has not been deleted?
pub(crate) async fn is_nest_mailbox(state: &AppState, address: &str) -> bool {
    let address = address.to_string();
    state
        .store
        .group_read(move |c| -> GResult<bool> {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM nests WHERE address = ?1 AND deleted_at IS NULL)",
                [&address],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap_or(false)
}

/// The KEK file named by `POSTBOX_NESTS_KEK_FILE`, if it holds exactly 64 hex characters.
fn read_kek() -> Option<zeroize::Zeroizing<String>> {
    let path = std::env::var("POSTBOX_NESTS_KEK_FILE").ok()?;
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(path.trim()).ok()?);
    let hex = text.trim();
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(zeroize::Zeroizing::new(hex.to_ascii_lowercase()))
    } else {
        tracing::warn!(
            "POSTBOX_NESTS_KEK_FILE does not hold a 64-hex-character key; Nests stay locked"
        );
        None
    }
}

/// Deleted Nests nestd must not keep (or bring back from a restore): the newest 1000.
async fn tombstones(state: &AppState) -> Vec<String> {
    state
        .store
        .group_read(|c| -> GResult<Vec<String>> {
            let mut stmt = c.prepare(
                "SELECT id FROM nests WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC LIMIT 1000",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap_or_default()
}

/// One unlock call: release the KEK and replay tombstones. Idempotent on nestd.
pub(crate) async fn unlock_once(state: &AppState) -> Result<Value, NestdError> {
    let Some(kek) = state.nests.kek.as_ref() else {
        return Err(NestdError::Unavailable);
    };
    let body = json!({ "kek": kek.as_str(), "tombstones": tombstones(state).await });
    match state
        .nests
        .call(reqwest::Method::POST, "/v1/unlock", Some(body))
        .await?
    {
        (200, v) => Ok(v),
        (status, v) => Err(NestdError::Failed(format!(
            "nestd answered {status}: {}",
            v["error"].as_str().unwrap_or("error")
        ))),
    }
}

/// Phase B6: keep the Nests host unlocked and its deletions in line with ours. Every minute, so a
/// rebooted host waits at most a minute, and a restored host destroys revoked Nests promptly.
pub(crate) fn start_unlocker(state: AppState) {
    if !state.nests.configured() || state.nests.kek.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut was_ok = false;
        loop {
            timer.tick().await;
            match unlock_once(&state).await {
                Ok(v) => {
                    if !was_ok {
                        tracing::info!("Nests host unlocked");
                    }
                    if let Some(t) = v["tombstoned"].as_array().filter(|t| !t.is_empty()) {
                        tracing::warn!(
                            count = t.len(),
                            "Nests host destroyed Nests the postbox had deleted"
                        );
                    }
                    was_ok = true;
                }
                Err(e) => {
                    let detail = match e {
                        NestdError::Unavailable => "not configured".to_string(),
                        NestdError::Failed(d) => d,
                    };
                    tracing::warn!(%detail, "Nests host unlock failed");
                    was_ok = false;
                }
            }
        }
    });
}

// ---- wake on mail -------------------------------------------------------------------------------

/// Mail just landed in `address`. If that mailbox is a live Nest, ask nestd to wake it — spawned,
/// debounced per Nest, and never able to fail or delay the send that called it.
pub(crate) fn wake_for(state: &AppState, address: &str) {
    if !state.nests.configured() {
        return;
    }
    let state = state.clone();
    let address = address.to_string();
    tokio::spawn(async move {
        let found = state
            .store
            .group_read(move |c| -> GResult<Option<String>> {
                Ok(c.query_row(
                    "SELECT id FROM nests WHERE address = ?1 AND deleted_at IS NULL",
                    [&address],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await;
        let Ok(Some(nest_id)) = found else {
            return;
        };
        let now = now_unix();
        {
            let mut woken = state.nests.woken.lock().expect("wake lock");
            if woken.len() > 10_000 {
                woken.retain(|_, at| *at + WAKE_DEBOUNCE_SECS > now);
            }
            if woken
                .get(&nest_id)
                .is_some_and(|at| *at + WAKE_DEBOUNCE_SECS > now)
            {
                return;
            }
            woken.insert(nest_id.clone(), now);
        }
        match state
            .nests
            .call(
                reqwest::Method::POST,
                &format!("/v1/nests/{nest_id}/wake"),
                Some(json!({})),
            )
            .await
        {
            Ok((status, _)) if (200..300).contains(&status) => {}
            Ok((status, _)) => tracing::warn!(nest = %nest_id, status, "Nest wake refused"),
            Err(_) => tracing::warn!(nest = %nest_id, "Nest wake failed"),
        }
    });
}

// ---- routers ------------------------------------------------------------------------------------

/// Public: the one route a sandbox calls, with no credential but its code and device key.
pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/v1/nests/enroll", post(enroll))
}

/// Internal: ops-token lifecycle and entitlement.
pub(crate) fn internal_router() -> Router<AppState> {
    Router::new()
        .route("/internal/v1/nests", get(list_nests).post(create_nest))
        .route("/internal/v1/nests/entitlement", get(entitlement))
        .route("/internal/v1/nests/{id}", get(get_nest).delete(delete_nest))
        .route("/internal/v1/nests/{id}/{action}", post(nest_action))
}

// ---- helpers ------------------------------------------------------------------------------------

pub(crate) type Peer = Option<Extension<ConnectInfo<std::net::SocketAddr>>>;

fn service(state: &AppState, headers: &HeaderMap) -> Result<&'static str, Box<Response>> {
    crate::internal_api::authorize_service(
        state,
        headers,
        crate::internal_api::Endpoint::Nests,
        Action::ManageNests,
    )
}

fn account_ok(id: &str) -> bool {
    id.starts_with("acct_")
        && id.len() <= 64
        && id[5..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn nest_id_ok(id: &str) -> bool {
    id.starts_with("nst_") && id.len() <= 64 && id[4..].chars().all(|c| c.is_ascii_hexdigit())
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn audit_row(
    state: &AppState,
    headers: &HeaderMap,
    peer: Peer,
    action: &str,
    nest_id: &str,
    details: Value,
) -> store::AuditRow {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    state.auditor.row(
        ActorKind::System,
        None,
        action,
        Some(format!("nest:{nest_id}")),
        details,
        peer.map(|Extension(ConnectInfo(addr))| addr.ip()),
        user_agent,
        now_unix(),
    )
}

/// One stored Nest.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct NestRow {
    pub nest_id: String,
    pub account_id: String,
    pub address: Option<String>,
    pub label: String,
    pub state: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

const NEST_COLUMNS: &str =
    "id, account_id, address, label, state, created_at, updated_at, deleted_at";

fn nest_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<NestRow> {
    Ok(NestRow {
        nest_id: r.get(0)?,
        account_id: r.get(1)?,
        address: r.get(2)?,
        label: r.get(3)?,
        state: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        deleted_at: r.get(7)?,
    })
}

pub(crate) async fn load_nest(state: &AppState, id: String) -> GResult<Option<NestRow>> {
    state
        .store
        .group_read(move |c| -> GResult<Option<NestRow>> {
            Ok(c.query_row(
                &format!("SELECT {NEST_COLUMNS} FROM nests WHERE id = ?1"),
                [&id],
                nest_from,
            )
            .optional()?)
        })
        .await
}

fn nest_missing() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "no_nest", "no such Nest")
}

pub(crate) fn set_state_in(tx: &Transaction<'_>, id: &str, state: &str, now: u64) -> GResult<()> {
    tx.execute(
        "UPDATE nests SET state = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, state, now as i64],
    )?;
    Ok(())
}

/// Replace a mailbox's capability token with the hash of bytes nobody keeps: every token issued
/// for it so far stops working, and no new one exists.
fn revoke_mailbox_in(tx: &Transaction<'_>, address: &str) -> GResult<()> {
    let mut raw = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut raw);
    tx.execute(
        "UPDATE identities SET cap_hash = ?2 WHERE address = ?1",
        params![address, sha256(&raw).to_vec()],
    )?;
    Ok(())
}

fn active_count_in(c: &rusqlite::Connection, account: &str) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM nests WHERE account_id = ?1 AND deleted_at IS NULL",
        [account],
        |r| r.get(0),
    )
}

// ---- POST /internal/v1/nests ---------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    account_id: String,
    label: String,
    #[serde(default)]
    allow_senders: Vec<String>,
}

/// `POST /internal/v1/nests` — create a Nest for an account: reserve the slot, mint its mailbox,
/// grant the named same-account senders `answer_question`, issue a one-time enrollment code and
/// ask nestd to provision the sandbox.
async fn create_nest(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let service = match service(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    match create_inner(&state, &headers, peer, service, body).await {
        Ok(resp) => resp,
        Err(e) => e.into_response(),
    }
}

async fn create_inner(
    state: &AppState,
    headers: &HeaderMap,
    peer: Peer,
    service: &'static str,
    body: Result<Json<Value>, JsonRejection>,
) -> GResult<Response> {
    let (req, _): (CreateReq, String) = parse(body)?;
    let account = req.account_id.trim().to_string();
    if !account_ok(&account) {
        return Err(GErr::bad("bad_account", "account_id is not an account id"));
    }
    let label = req.label.trim().to_string();
    if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err(GErr::bad("bad_label", "label: 1-64 printable characters"));
    }
    if req.allow_senders.len() > MAX_ALLOW_SENDERS {
        return Err(GErr::bad("bad_sender", "at most 8 allow_senders"));
    }
    if !state.nests.configured() {
        return Err(GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "nests_unavailable",
            "no Nest worker host is configured",
        ));
    }
    if !crate::filtering::feature_on(state, Some(&account), capabilities::NESTS).await {
        return Err(GErr::forbidden(
            "feature_disabled",
            "Nests are not enabled for this account",
        ));
    }
    // Phase A never auto-grants a stranger: every sender must be a mailbox of the same account.
    let mut senders = Vec::new();
    for sender in &req.allow_senders {
        let sender = sender.trim().to_string();
        let owned = state
            .store
            .get(sender.clone())
            .await
            .map_err(GErr::from)?
            .is_some_and(|id| id.account_id.as_deref() == Some(account.as_str()));
        if !owned {
            return Err(GErr::bad(
                "bad_sender",
                "allow_senders must be /k/ mailboxes of the same account",
            ));
        }
        if !senders.contains(&sender) {
            senders.push(sender);
        }
    }

    let max = state.nests.max_per_account;
    let (nest_id, address, new_state) = provision(
        state,
        Provision {
            account,
            label,
            senders,
            funding: Funding::Ops { max },
            budgets: (
                state.nests.input_tokens as i64,
                state.nests.output_tokens as i64,
            ),
            quota_gib: None,
            audit_service: Some(service),
            address: None,
        },
        Some((headers, peer)),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "nest_id": nest_id, "address": address, "state": new_state })),
    )
        .into_response())
}

/// How a new Nest is paid for, and therefore which limit admits it.
pub(crate) enum Funding {
    /// Phase A ops path: the per-account limit `POSTBOX_NESTS_MAX_PER_ACCOUNT`.
    Ops { max: i64 },
    /// A Nest package: a free slot in the account's allowance (Nests Phase B1), checked in the
    /// same transaction that reserves it.
    Package,
}

pub(crate) struct Provision {
    pub account: String,
    pub label: String,
    /// Same-account mailboxes granted `answer_question` (already validated by the caller).
    pub senders: Vec<String>,
    pub funding: Funding,
    /// Monthly model budget for this Nest (input, output tokens).
    pub budgets: (i64, i64),
    pub quota_gib: Option<u64>,
    pub audit_service: Option<&'static str>,
    /// An existing mailbox to run the Nest as (an organization's agent, Nests B7) instead of
    /// minting one. Its credential is still rotated by the enrollment, as for a minted one.
    pub address: Option<String>,
}

/// Create one Nest: reserve the slot, mint its mailbox, grant the named senders, issue the one-time
/// enrollment code and ask nestd to provision the sandbox. Shared by the ops route, the member
/// route and the paid-setup reconciler.
pub(crate) async fn provision(
    state: &AppState,
    p: Provision,
    request: Option<(&HeaderMap, Peer)>,
) -> GResult<(String, String, &'static str)> {
    let Provision {
        account,
        label,
        senders,
        funding,
        budgets,
        quota_gib,
        audit_service,
        address: existing,
    } = p;
    if !state.nests.configured() {
        return Err(GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "nests_unavailable",
            "no Nest worker host is configured",
        ));
    }
    // 1. Reserve the slot: the count and the insert are one transaction.
    let nest_id = format!("nst_{}", rand_hex(16));
    let now = now_unix();
    let funding_kind = match funding {
        Funding::Ops { .. } => "ops",
        Funding::Package => "package",
    };
    {
        let (nest_id, account, label) = (nest_id.clone(), account.clone(), label.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let exists: Option<i64> = tx
                    .query_row("SELECT 1 FROM accounts WHERE id = ?1", [&account], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if exists.is_none() {
                    return Err(GErr::new(
                        StatusCode::NOT_FOUND,
                        "no_account",
                        "no such account",
                    ));
                }
                match funding {
                    Funding::Ops { max } => {
                        if active_count_in(tx, &account)? >= max {
                            return Err(GErr::conflict(
                                "nest_limit",
                                format!("this account already holds its limit of {max} Nest(s)"),
                            )
                            .with("max", max));
                        }
                    }
                    Funding::Package => {
                        let allowance = store::nest_funding::allowance(tx, &account, now)
                            .map_err(GErr::from)?;
                        if allowance.used >= allowance.slots {
                            return Err(GErr::conflict(
                                "nest_slots_full",
                                "every Nest slot of this account's package is in use",
                            )
                            .with("slots", allowance.slots)
                            .with("used", allowance.used));
                        }
                    }
                }
                tx.execute(
                    "INSERT INTO nests (id, account_id, address, label, state, created_at, updated_at, funding)
                     VALUES (?1, ?2, NULL, ?3, 'creating', ?4, ?4, ?5)",
                    params![nest_id, account, label, now as i64, funding_kind],
                )?;
                Ok(())
            })
            .await?;
    }

    // 2. The mailbox. Its capability token is dropped here, unread: nobody ever holds it.
    let minted = match existing {
        Some(a) => Ok(json!({ "address": a })),
        None => {
            crate::do_create_identity(
                state,
                Some(account.clone()),
                Some(format!("nest: {label}")),
                None,
            )
            .await
        }
    };
    let address = match minted {
        Ok(v) => v["address"].as_str().unwrap_or_default().to_string(),
        Err(e) => {
            let id = nest_id.clone();
            let _ = state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute(
                        "UPDATE nests SET state = 'failed', deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
                        params![id, now as i64],
                    )?;
                    Ok(())
                })
                .await;
            return Err(e.into());
        }
    };

    // The row names its mailbox before any grant is written: a `chat` grant is only accepted on a
    // mailbox that is a Nest.
    {
        let (id, addr) = (nest_id.clone(), address.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nests SET address = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, addr, now as i64],
                )?;
                Ok(())
            })
            .await?;
    }

    // 3. The named senders, granted what a Nest answers (questions and conversation).
    let me = state
        .store
        .get(address.clone())
        .await
        .map_err(GErr::from)?
        .ok_or_else(|| {
            GErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "store_error",
            )
        })?;
    for sender in &senders {
        crate::do_set_contact(
            state,
            &me,
            sender.clone(),
            None,
            Some(crate::ADMISSION_ALLOW.to_string()),
            Some(crate::AUTONOMY_AUTO.to_string()),
            Some(GRANTED_VERBS.iter().map(|v| v.to_string()).collect()),
            crate::TrustActor::Human,
        )
        .await?;
    }

    // 4. The one-time code, stored only as a hash, and the audit record.
    let code = format!("nenr_{}", rand_hex(32));
    {
        let details = json!({ "service": audit_service, "account": account, "address": address, "senders": senders.len(), "funding": funding_kind });
        let row = match request {
            Some((headers, peer)) => {
                audit_row(state, headers, peer, "nest.create", &nest_id, details)
            }
            None => state.auditor.row(
                ActorKind::System,
                Some(account.clone()),
                "nest.create",
                Some(format!("nest:{nest_id}")),
                details,
                None,
                None,
                now_unix(),
            ),
        };
        let (nest_id, address, code_hash) =
            (nest_id.clone(), address.clone(), sha256(code.as_bytes()));
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nests SET address = ?2, state = 'enrolling', updated_at = ?3 WHERE id = ?1",
                    params![nest_id, address, now as i64],
                )?;
                tx.execute(
                    "INSERT INTO nest_enrollments (id, nest_id, code_hash, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        format!("nenrid_{}", rand_hex(8)),
                        nest_id,
                        code_hash.to_vec(),
                        now as i64,
                        (now + CODE_SECS) as i64
                    ],
                )?;
                crate::store::audit::append_in(tx, &row)?;
                Ok(())
            })
            .await?;
    }

    // 5. Ask nestd to provision. The code travels once, over the private link.
    let payload = json!({
        "nest_id": nest_id,
        "account_id": account,
        "address": address,
        "label": label,
        "enrollment_code": code,
        "budgets": {
            "input_tokens": budgets.0,
            "output_tokens": budgets.1,
        },
        "quota_gib": quota_gib,
    });
    let answer = state
        .nests
        .call(reqwest::Method::POST, "/v1/nests", Some(payload))
        .await;
    let ok = matches!(&answer, Ok((status, _)) if (200..300).contains(status));
    let id = nest_id.clone();
    let new_state = if ok { "provisioning" } else { "failed" };
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            set_state_in(tx, &id, new_state, now_unix())?;
            if !ok {
                tx.execute(
                    "UPDATE nest_enrollments SET revoked_at = ?2 WHERE nest_id = ?1 AND redeemed_at IS NULL AND revoked_at IS NULL",
                    params![id, now_unix() as i64],
                )?;
            }
            Ok(())
        })
        .await?;
    if !ok {
        let detail = match answer {
            Ok((status, _)) => format!("nestd answered {status}"),
            Err(NestdError::Unavailable) => "nestd is not configured".to_string(),
            Err(NestdError::Failed(e)) => e,
        };
        tracing::warn!(nest = %nest_id, %detail, "Nest provisioning failed");
        return Err(GErr::new(
            StatusCode::BAD_GATEWAY,
            "nestd_failed",
            "the worker host did not provision the Nest; delete it and retry",
        )
        .with("nest_id", nest_id));
    }
    tracing::info!(nest = %nest_id, address = %address, "Nest created");
    Ok((nest_id, address, new_state))
}

// ---- reads --------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct AccountQuery {
    account_id: String,
}

/// `GET /internal/v1/nests?account_id=` — the account's Nests, deleted ones included.
async fn list_nests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Response {
    if let Err(resp) = service(&state, &headers) {
        return *resp;
    }
    if !account_ok(&q.account_id) {
        return GErr::bad("bad_account", "account_id is not an account id").into_response();
    }
    let account = q.account_id;
    let rows = state
        .store
        .group_read(move |c| -> GResult<Vec<NestRow>> {
            let mut stmt = c.prepare(&format!(
                "SELECT {NEST_COLUMNS} FROM nests WHERE account_id = ?1 ORDER BY created_at, id"
            ))?;
            let rows = stmt.query_map([&account], nest_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await;
    match rows {
        Ok(rows) => Json(json!({ "nests": rows })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /internal/v1/nests/entitlement?account_id=` — may this account have another Nest?
async fn entitlement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Response {
    if let Err(resp) = service(&state, &headers) {
        return *resp;
    }
    if !account_ok(&q.account_id) {
        return GErr::bad("bad_account", "account_id is not an account id").into_response();
    }
    let enabled =
        crate::filtering::feature_on(&state, Some(&q.account_id), capabilities::NESTS).await;
    let account = q.account_id.clone();
    let active = state
        .store
        .group_read(move |c| -> GResult<i64> { Ok(active_count_in(c, &account)?) })
        .await;
    match active {
        Ok(active) => Json(json!({
            "account_id": q.account_id,
            "enabled": enabled,
            "max": state.nests.max_per_account,
            "active": active,
        }))
        .into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /internal/v1/nests/{id}` — the stored row, plus nestd's runtime view when it answers.
async fn get_nest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = service(&state, &headers) {
        return *resp;
    }
    if !nest_id_ok(&id) {
        return nest_missing().into_response();
    }
    let row = match load_nest(&state, id.clone()).await {
        Ok(Some(row)) => row,
        Ok(None) => return nest_missing().into_response(),
        Err(e) => return e.into_response(),
    };
    let runtime = if row.deleted_at.is_some() {
        Value::Null
    } else {
        match state
            .nests
            .call(reqwest::Method::GET, &format!("/v1/nests/{id}"), None)
            .await
        {
            Ok((200, body)) => body,
            Ok((status, _)) => json!({ "error": "nestd_status", "status": status }),
            Err(NestdError::Unavailable) => json!({ "error": "nests_unavailable" }),
            Err(NestdError::Failed(_)) => json!({ "error": "nestd_unreachable" }),
        }
    };
    let mut body = serde_json::to_value(&row).unwrap_or(Value::Null);
    body["runtime"] = runtime;
    Json(body).into_response()
}

// ---- lifecycle ----------------------------------------------------------------------------------

/// `POST /internal/v1/nests/{id}/{start|stop|pause|resume}` — forwarded to nestd. Start and resume
/// re-check the flag, so turning `nests` off for an account stops it being woken again.
async fn nest_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    Path((id, action)): Path<(String, String)>,
) -> Response {
    let service = match service(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    let result = async {
        let next = match action.as_str() {
            "start" | "resume" => "running",
            "stop" => "stopped",
            "pause" => "paused",
            _ => {
                return Err(GErr::new(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "unknown action",
                ))
            }
        };
        if !nest_id_ok(&id) {
            return Err(nest_missing());
        }
        let row = load_nest(&state, id.clone())
            .await?
            .filter(|r| r.deleted_at.is_none())
            .ok_or_else(nest_missing)?;
        if matches!(action.as_str(), "start" | "resume")
            && !crate::filtering::feature_on(&state, Some(&row.account_id), capabilities::NESTS)
                .await
        {
            return Err(GErr::forbidden(
                "feature_disabled",
                "Nests are not enabled for this account",
            ));
        }
        let runtime = runtime_action(&state, &id, &action).await?;
        let audit = audit_row(
            &state,
            &headers,
            peer,
            &format!("nest.{action}"),
            &id,
            json!({ "service": service }),
        );
        let nid = id.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                set_state_in(tx, &nid, next, now_unix())?;
                crate::store::audit::append_in(tx, &audit)?;
                Ok(())
            })
            .await?;
        Ok(Json(json!({ "nest_id": id, "state": next, "runtime": runtime })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// Ask nestd to `start|stop|pause|resume` one Nest. The caller records the new state.
pub(crate) async fn runtime_action(state: &AppState, id: &str, action: &str) -> GResult<Value> {
    let (status, runtime) = match state
        .nests
        .call(
            reqwest::Method::POST,
            &format!("/v1/nests/{id}/{action}"),
            None,
        )
        .await
    {
        Ok(answer) => answer,
        Err(NestdError::Unavailable) => {
            return Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "nests_unavailable",
                "no Nest worker host is configured",
            ))
        }
        Err(NestdError::Failed(_)) => {
            return Err(GErr::new(
                StatusCode::BAD_GATEWAY,
                "nestd_failed",
                "the worker host did not answer",
            ))
        }
    };
    if !(200..300).contains(&status) {
        return Err(GErr::new(
            StatusCode::BAD_GATEWAY,
            "nestd_failed",
            format!("the worker host answered {status}"),
        )
        .with("runtime", runtime));
    }
    Ok(runtime)
}

/// Tear one Nest down in nestd, then revoke its mailbox credential and open codes and mark the row
/// deleted, with `audit` in the same transaction. Nothing is revoked if nestd did not confirm.
pub(crate) async fn teardown(
    state: &AppState,
    id: &str,
    address: Option<String>,
    audit: store::AuditRow,
) -> GResult<()> {
    match state
        .nests
        .call(reqwest::Method::DELETE, &format!("/v1/nests/{id}"), None)
        .await
    {
        Ok((status, _)) if (200..300).contains(&status) || status == 404 => {}
        Ok((status, _)) => {
            return Err(GErr::new(
                StatusCode::BAD_GATEWAY,
                "nestd_failed",
                format!("the worker host answered {status}; nothing was revoked, retry"),
            ))
        }
        Err(NestdError::Unavailable) => {
            return Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "nests_unavailable",
                "no Nest worker host is configured",
            ))
        }
        Err(NestdError::Failed(_)) => {
            return Err(GErr::new(
                StatusCode::BAD_GATEWAY,
                "nestd_failed",
                "the worker host did not answer; nothing was revoked, retry",
            ))
        }
    }
    let nid = id.to_string();
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let now = now_unix() as i64;
            if let Some(address) = &address {
                revoke_mailbox_in(tx, address)?;
            }
            tx.execute(
                "UPDATE nest_enrollments SET revoked_at = ?2 WHERE nest_id = ?1 AND revoked_at IS NULL",
                params![nid, now],
            )?;
            tx.execute(
                "UPDATE nests SET state = 'deleted', deleted_at = COALESCE(deleted_at, ?2), updated_at = ?2 WHERE id = ?1",
                params![nid, now],
            )?;
            crate::store::audit::append_in(tx, &audit)?;
            Ok(())
        })
        .await?;
    tracing::info!(nest = %id, "Nest deleted; mailbox credential revoked");
    Ok(())
}

/// `DELETE /internal/v1/nests/{id}` — tear down the runtime, revoke the mailbox credential and
/// any open code, and mark the row deleted. Idempotent: deleting a deleted Nest answers 200.
async fn delete_nest(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    Path(id): Path<String>,
) -> Response {
    let service = match service(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    let result = async {
        if !nest_id_ok(&id) {
            return Err(nest_missing());
        }
        let row = load_nest(&state, id.clone())
            .await?
            .ok_or_else(nest_missing)?;
        if row.deleted_at.is_some() && row.state == "deleted" {
            return Ok(Json(json!({ "nest_id": id, "state": "deleted" })).into_response());
        }
        let audit = audit_row(
            &state,
            &headers,
            peer,
            "nest.delete",
            &id,
            json!({ "service": service, "address": row.address }),
        );
        teardown(&state, &id, row.address.clone(), audit).await?;
        Ok(Json(json!({ "nest_id": id, "state": "deleted" })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

// ---- POST /v1/nests/enroll ------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollReq {
    code: String,
    device_public_key: String,
    signature: String,
}

/// Every refusal looks the same, so the route never says whether a code existed.
fn enrollment_invalid() -> GErr {
    GErr::bad("enrollment_invalid", "that enrollment code is not valid")
}

fn device_key(s: &str) -> GResult<VerifyingKey> {
    let raw = crate::groups::unb64("device_public_key", s.trim())?;
    let arr: [u8; 32] = raw.try_into().map_err(|_| enrollment_invalid())?;
    VerifyingKey::from_bytes(&arr).map_err(|_| enrollment_invalid())
}

fn signature_ok(key: &VerifyingKey, code: &str, sig_b64: &str) -> bool {
    let Ok(raw) = crate::groups::unb64("signature", sig_b64.trim()) else {
        return false;
    };
    let Ok(arr): Result<[u8; 64], _> = raw.try_into() else {
        return false;
    };
    let msg = format!("{ENROLL_CONTEXT}{code}");
    key.verify(msg.as_bytes(), &Signature::from_bytes(&arr))
        .is_ok()
}

/// `POST /v1/nests/enroll` — no credential. The sandbox proves its device key over the one-time
/// code; the mailbox's capability token is rotated and the new one returned to it alone.
async fn enroll(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        rate_limit(&format!("nest_enroll:{}", cip.as_str()), 10, 600).map_err(|e| GErr {
            code: "enrollment_rate_limited",
            ..e
        })?;
        let (req, _): (EnrollReq, String) = parse(body).map_err(|_| enrollment_invalid())?;
        let code = req.code.trim().to_string();
        if !code.starts_with("nenr_") || code.len() > 100 {
            return Err(enrollment_invalid());
        }
        let key = device_key(&req.device_public_key)?;
        if !signature_ok(&key, &code, &req.signature) {
            return Err(enrollment_invalid());
        }
        let facts = Facts {
            now: now_unix(),
            device_proof: true,
            ..Facts::default()
        };
        authorize(
            &Actor::Anonymous,
            Action::DeviceEnroll,
            &Resource::DeviceGrant("nest"),
            &facts,
        )
        .map_err(|_| enrollment_invalid())?;

        // Which account the code belongs to, so the flag can be checked before anything changes.
        let code_hash = sha256(code.as_bytes()).to_vec();
        let lookup = code_hash.clone();
        let account: Option<String> = state
            .store
            .group_read(move |c| -> GResult<Option<String>> {
                Ok(c.query_row(
                    "SELECT n.account_id FROM nest_enrollments e JOIN nests n ON n.id = e.nest_id WHERE e.code_hash = ?1",
                    [lookup],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await?;
        let Some(account) = account else {
            return Err(enrollment_invalid());
        };
        if !crate::filtering::feature_on(&state, Some(&account), capabilities::NESTS).await {
            return Err(enrollment_invalid());
        }

        let ip_hash = state.auditor.ip_hash(
            cip.as_str()
                .parse()
                .unwrap_or(std::net::IpAddr::from([0, 0, 0, 0])),
        );
        let audit_ip = cip.as_str().parse().ok();
        let auditor = state.auditor.clone();
        let now = now_unix();
        let key_bytes = key.to_bytes().to_vec();
        let (nest_id, address, token) = state
            .store
            .group_tx(move |tx| -> GResult<(String, String, String)> {
                let row: Option<EnrollRow> = tx
                    .query_row(
                        "SELECT e.id, e.nest_id, e.expires_at, e.redeemed_at, e.revoked_at, n.address, n.deleted_at
                         FROM nest_enrollments e JOIN nests n ON n.id = e.nest_id WHERE e.code_hash = ?1",
                        [code_hash],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                    )
                    .optional()?;
                let Some((eid, nest_id, expires, redeemed, revoked, Some(address), None)) = row else {
                    return Err(enrollment_invalid());
                };
                if redeemed.is_some() || revoked.is_some() || expires <= now as i64 {
                    return Err(enrollment_invalid());
                }
                let token = gen_cap_token();
                let changed = tx.execute(
                    "UPDATE identities SET cap_hash = ?2 WHERE address = ?1",
                    params![address, sha256(token.as_bytes()).to_vec()],
                )?;
                if changed != 1 {
                    return Err(enrollment_invalid());
                }
                tx.execute(
                    "UPDATE nest_enrollments SET redeemed_at = ?2, device_public_key = ?3, redeemed_ip_hash = ?4 WHERE id = ?1",
                    params![eid, now as i64, key_bytes, ip_hash],
                )?;
                tx.execute(
                    "UPDATE nests SET state = CASE WHEN state IN ('enrolling', 'provisioning') THEN 'ready' ELSE state END, updated_at = ?2 WHERE id = ?1",
                    params![nest_id, now as i64],
                )?;
                let row = auditor.row(
                    ActorKind::System,
                    None,
                    "nest.enroll",
                    Some(format!("nest:{nest_id}")),
                    json!({ "enrollment": eid, "device_key": hex_str(&key_bytes) }),
                    audit_ip,
                    None,
                    now,
                );
                crate::store::audit::append_in(tx, &row)?;
                Ok((nest_id, address, token))
            })
            .await?;
        tracing::info!(nest = %nest_id, "Nest enrolled");
        Ok(Json(json!({
            "nest_id": nest_id,
            "address": address,
            "capability_token": token,
        }))
        .into_response())
    }
    .await;
    no_store(result.unwrap_or_else(|e: GErr| e.into_response()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_private_or_tls_urls_are_accepted() {
        assert!(url_ok("https://nests.example"));
        assert!(url_ok("http://10.66.0.2:8790"));
        assert!(url_ok("http://127.0.0.1:8790"));
        assert!(url_ok("http://192.168.1.4/x"));
        assert!(!url_ok("http://45.132.181.100:8790"));
        assert!(!url_ok("http://nests.example"));
        assert!(!url_ok("ftp://10.0.0.1"));
        assert!(!url_ok("https://"));
    }

    #[test]
    fn ids_are_checked_before_use() {
        assert!(nest_id_ok("nst_0123abcd"));
        assert!(!nest_id_ok("nst_../x"));
        assert!(!nest_id_ok("acct_1"));
        assert!(account_ok("acct_abc_1"));
        assert!(!account_ok("acct_a/b"));
    }
}
