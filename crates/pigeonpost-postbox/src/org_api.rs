//! `/org/v1` — the enterprise API (SDS §18; contract `docs/org-api.md`, spec
//! `openapi/pigeonpost-org.v1.yaml`).
//!
//! Every request passes [`gate`] first: the org API key is authenticated (`org_api_keys`), the
//! per-key rate limits are applied, the body is read once so `Idempotency-Key` can be checked
//! against a hash of the exact request, and — after the handler — the use is audited with the key
//! id. Handlers then ask `KeyCtx::require(scope, perms)`, which goes through `policy::authorize`
//! (scope ∧ the creator's live authority ∧ the org's state), and apply the operation's own bounds.
//!
//! No handler here reads message content, group ciphertext, attachments or keys (D-04).

// Row tuples read straight from SQL are clearer inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

use crate::audit::ActorKind;
use crate::groups::{parse, rate_limit, GErr, GResult};
use crate::org::{self, load_member, load_org, roles_of, MemberRow, Perm, Role};
use crate::org_api_keys::{self, KeyCtx};
use crate::{capabilities, now_unix, AppState, ClientIp};
use axum::body::Body as AxBody;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{MatchedPath, Path, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// How long an `Idempotency-Key` replay record lives.
pub(crate) const IDEMPOTENCY_SECS: i64 = 86_400;
/// Requests per key per minute, and writes among them (SDS §18.2).
const RATE_ALL: u32 = 600;
const RATE_WRITES: u32 = 60;
const RATE_WINDOW: u64 = 60;
/// The largest request body the API reads.
const MAX_BODY: usize = 256 * 1024;
/// The largest direct message `POST /messages` sends.
const MAX_MESSAGE: usize = 64 * 1024;
const DOC_BASE: &str = "https://developers.pigeonpost.dev/org-api-errors#";

// ---- conventions -------------------------------------------------------------------------------

/// Unix seconds as ISO 8601 UTC (`2026-10-27T10:15:00Z`), or `null`.
pub(crate) fn iso(t: Option<i64>) -> Value {
    t.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|d| json!(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
}

/// A `LIKE` pattern matching names that start with `prefix` literally (`api_key.` must not let
/// `_` match any character).
pub(crate) fn like_prefix(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for ch in prefix.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

fn parse_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|d| d.timestamp())
}

/// An API error in the stable schema `{"error": {"code", "message", "doc_url"}}`.
pub(crate) fn api_error(e: GErr) -> Response {
    let mut err = json!({
        "code": e.code,
        "message": e.message,
        "doc_url": format!("{DOC_BASE}{}", e.code),
    });
    if !e.extra.is_empty() {
        err["details"] = Value::Object(e.extra.clone());
    }
    let mut resp = (e.status, Json(json!({ "error": err }))).into_response();
    json_charset(&mut resp);
    if let Some(secs) = e.retry_after {
        if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
    }
    if let Some(v) = e.challenge.and_then(|c| HeaderValue::from_str(&c).ok()) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

fn json_charset(resp: &mut Response) {
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
}

/// What a handler decided.
#[derive(Default)]
pub(crate) struct ApiOut {
    status: u16,
    body: Value,
    wake: Vec<String>,
    no_store: bool,
    replayed: bool,
}

impl ApiOut {
    fn data(v: Value) -> Self {
        ApiOut {
            status: 200,
            body: json!({ "data": v }),
            ..ApiOut::default()
        }
    }
    fn created(v: Value) -> Self {
        ApiOut {
            status: 201,
            body: json!({ "data": v }),
            ..ApiOut::default()
        }
    }
    fn list(items: Vec<Value>, next: Option<String>) -> Self {
        ApiOut {
            status: 200,
            body: json!({ "data": items, "next_cursor": next }),
            ..ApiOut::default()
        }
    }
    fn secret(mut self) -> Self {
        self.no_store = true;
        self
    }
}

fn respond(state: &AppState, result: GResult<ApiOut>) -> Response {
    match result {
        Ok(out) => {
            for a in &out.wake {
                state.event_hub.wake(a);
            }
            let status = StatusCode::from_u16(out.status).unwrap_or(StatusCode::OK);
            let mut resp = (status, Json(out.body)).into_response();
            json_charset(&mut resp);
            if out.no_store {
                resp.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            }
            if out.replayed {
                resp.headers_mut()
                    .insert("idempotent-replayed", HeaderValue::from_static("true"));
            }
            resp
        }
        Err(e) => api_error(e),
    }
}

fn bad(code: &'static str, message: impl Into<String>) -> GErr {
    GErr::bad(code, message)
}

fn not_found(what: &str) -> GErr {
    GErr::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("no such {what}"),
    )
}

fn human_required(what: &str) -> GErr {
    GErr::forbidden(
        "human_required",
        format!("{what} needs a person signed in to the dashboard; API keys cannot do it"),
    )
}

#[derive(Deserialize, Default)]
pub(crate) struct Page {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

fn page(q: Result<Query<Page>, QueryRejection>) -> GResult<Page> {
    q.map(|Query(p)| p)
        .map_err(|e| bad("bad_request", e.body_text()))
}

fn limit(p: &Page) -> GResult<i64> {
    match p.limit {
        None => Ok(50),
        Some(n) if (1..=200).contains(&n) => Ok(n),
        Some(_) => Err(bad("bad_request", "limit is 1 to 200")),
    }
}

/// Cursors are opaque to clients: a versioned, hex-encoded sort key.
fn encode_cursor(key: &str) -> String {
    crate::hex_str(format!("v1|{key}").as_bytes())
}

fn decode_cursor(p: &Page) -> GResult<Option<String>> {
    let Some(raw) = p.cursor.as_deref().filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let invalid = || bad("invalid_cursor", "this cursor is not one the API returned");
    if raw.len() % 2 != 0 || raw.len() > 1_024 {
        return Err(invalid());
    }
    let bytes: Option<Vec<u8>> = (0..raw.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).ok())
        .collect();
    let text = bytes
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(invalid)?;
    text.strip_prefix("v1|")
        .map(|k| Some(k.to_string()))
        .ok_or_else(invalid)
}

/// Split `limit + 1` rows into a page and the next cursor.
fn paginate(mut rows: Vec<(String, Value)>, limit: i64) -> (Vec<Value>, Option<String>) {
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next = if more {
        rows.last().map(|(k, _)| encode_cursor(k))
    } else {
        None
    };
    (rows.into_iter().map(|(_, v)| v).collect(), next)
}

// ---- rate limits -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct RateState {
    limit: u32,
    remaining: u32,
    reset: u64,
}

struct Bucket {
    start: u64,
    all: u32,
    writes: u32,
}

fn buckets() -> &'static Mutex<HashMap<String, Bucket>> {
    static B: OnceLock<Mutex<HashMap<String, Bucket>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Count one request against the key's window. `Err` carries the state for a `429`.
fn hit(key: &str, write: bool, now: u64) -> Result<RateState, RateState> {
    let mut map = buckets().lock().expect("rate lock");
    if map.len() > 20_000 {
        map.retain(|_, b| b.start + RATE_WINDOW > now);
    }
    let b = map.entry(key.to_string()).or_insert(Bucket {
        start: now,
        all: 0,
        writes: 0,
    });
    if now >= b.start + RATE_WINDOW {
        *b = Bucket {
            start: now,
            all: 0,
            writes: 0,
        };
    }
    let reset = (b.start + RATE_WINDOW).saturating_sub(now).max(1);
    let (limit, used) = if write {
        (RATE_WRITES, b.writes)
    } else {
        (RATE_ALL, b.all)
    };
    if b.all >= RATE_ALL || (write && b.writes >= RATE_WRITES) {
        return Err(RateState {
            limit,
            remaining: 0,
            reset,
        });
    }
    b.all += 1;
    if write {
        b.writes += 1;
    }
    Ok(RateState {
        limit,
        remaining: limit - used - 1,
        reset,
    })
}

fn rate_headers(resp: &mut Response, r: RateState) {
    let h = resp.headers_mut();
    for (name, v) in [
        ("ratelimit-limit", r.limit as u64),
        ("ratelimit-remaining", r.remaining as u64),
        ("ratelimit-reset", r.reset),
    ] {
        if let Ok(v) = HeaderValue::from_str(&v.to_string()) {
            h.insert(name, v);
        }
    }
}

// ---- idempotency -------------------------------------------------------------------------------

/// An `Idempotency-Key` request: scoped to the key and the route, bound to a hash of the request.
#[derive(Clone)]
pub(crate) struct IdemReq {
    key_id: String,
    route: String,
    key: String,
    hash: String,
}

#[derive(Clone)]
pub(crate) struct Idem(Option<IdemReq>);

/// A recorded response for this request, if there is one: `Ok(Some)` to replay, `Err` when the key
/// was used for a different request.
fn replay_in(
    c: &Connection,
    vault: &crate::vault::Vault,
    i: &IdemReq,
    now: u64,
) -> GResult<Option<ApiOut>> {
    let prior: Option<(String, i64, Vec<u8>)> = c
        .query_row(
            "SELECT request_hash, status, response FROM org_api_idempotency
              WHERE key_id = ?1 AND route = ?2 AND idem_key = ?3 AND created_at > ?4",
            params![i.key_id, i.route, i.key, now as i64 - IDEMPOTENCY_SECS],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((hash, status, sealed)) = prior else {
        return Ok(None);
    };
    if hash != i.hash {
        return Err(GErr::conflict(
            "idempotency_conflict",
            "this Idempotency-Key was used for a different request",
        ));
    }
    let body = vault
        .open_bytes(&sealed)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    Ok(Some(ApiOut {
        status: status as u16,
        body,
        no_store: true,
        replayed: true,
        ..ApiOut::default()
    }))
}

/// Record a successful response (sealed: it may carry a one-time secret or invitation link).
fn record_in(
    c: &Connection,
    vault: &crate::vault::Vault,
    i: &IdemReq,
    out: &ApiOut,
    now: u64,
) -> GResult<()> {
    let sealed = vault
        .seal_bytes(out.body.to_string().as_bytes())
        .map_err(|_| {
            GErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "vault_error",
                "vault_error",
            )
        })?;
    c.execute(
        "INSERT OR REPLACE INTO org_api_idempotency (key_id, route, idem_key, request_hash, status, response, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![i.key_id, i.route, i.key, i.hash, out.status as i64, sealed, now as i64],
    )?;
    Ok(())
}

/// Run a write in one IMMEDIATE transaction, replaying or recording it under its
/// `Idempotency-Key`. Only successful responses are recorded.
async fn write<F>(state: &AppState, idem: &Idem, f: F) -> GResult<ApiOut>
where
    F: FnOnce(&Connection, u64) -> GResult<ApiOut> + Send + 'static,
{
    let vault = state.vault.clone();
    let idem = idem.0.clone();
    let now = now_unix();
    state
        .store
        .group_tx(move |tx| -> GResult<ApiOut> {
            if let Some(i) = &idem {
                if let Some(replay) = replay_in(tx, &vault, i, now)? {
                    return Ok(replay);
                }
            }
            let out = f(tx, now)?;
            if let Some(i) = &idem {
                record_in(tx, &vault, i, &out, now)?;
            }
            Ok(out)
        })
        .await
}

// ---- the gate ----------------------------------------------------------------------------------

/// Authenticate, rate-limit, read the body for idempotency, run the handler, audit the use.
async fn gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ClientIp>()
        .map(|c| c.as_str().to_string())
        .unwrap_or_default();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let method = req.method().clone();
    let write = !matches!(method, Method::GET | Method::HEAD);
    let ctx = match org_api_keys::authenticate(&state, crate::bearer(req.headers()), &ip).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            // Guessing keys is hopeless (160 bits), but a flood of bad ones still costs lookups.
            if e.status == StatusCode::UNAUTHORIZED {
                if let Err(limited) = rate_limit(&format!("orgapibad:{ip}"), 120, 60) {
                    return api_error(limited);
                }
            }
            return api_error(e);
        }
    };
    let rate = match hit(&ctx.key_id, write, now_unix()) {
        Ok(r) => r,
        Err(r) => {
            let mut e = GErr::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many requests for this key; slow down",
            );
            e.retry_after = Some(r.reset);
            let mut resp = api_error(e);
            rate_headers(&mut resp, r);
            return resp;
        }
    };
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            let mut resp = api_error(GErr::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "the request body is larger than 256 KB",
            ));
            rate_headers(&mut resp, rate);
            return resp;
        }
    };
    let idem = match parts.headers.get("idempotency-key") {
        Some(v) if write => {
            let key = v.to_str().unwrap_or("").trim().to_string();
            if key.is_empty() || key.len() > 255 || !key.bytes().all(|b| (0x20..0x7f).contains(&b))
            {
                let mut resp = api_error(bad(
                    "bad_request",
                    "Idempotency-Key is 1 to 255 printable ASCII characters",
                ));
                rate_headers(&mut resp, rate);
                return resp;
            }
            let path = parts.uri.path().to_string();
            let mut material =
                format!("{method}\n{path}\n{}\n", parts.uri.query().unwrap_or("")).into_bytes();
            material.extend_from_slice(&bytes);
            Some(IdemReq {
                key_id: ctx.key_id.clone(),
                route: format!("{method} {path}"),
                key,
                hash: crate::hex_str(&crate::sha256(&material)),
            })
        }
        _ => None,
    };
    let mut req = Request::from_parts(parts, AxBody::from(bytes));
    req.extensions_mut().insert(ctx.clone());
    req.extensions_mut().insert(Idem(idem));
    let mut resp = next.run(req).await;
    rate_headers(&mut resp, rate);
    record_use(
        &state,
        &ctx,
        method.as_str(),
        &route,
        resp.status().as_u16(),
        &ip,
    )
    .await;
    resp
}

/// Every authenticated use is audited with the key id (SDS §18.2); `last_used_at` is kept to the
/// minute.
async fn record_use(
    state: &AppState,
    ctx: &KeyCtx,
    method: &str,
    route: &str,
    status: u16,
    ip: &str,
) {
    let auditor = state.auditor.clone();
    let (org_id, key_id) = (ctx.org.id.clone(), ctx.key_id.clone());
    let route = route.trim_start_matches("/org/v1").to_string();
    let (method, ip) = (method.to_string(), ip.to_string());
    let now = now_unix();
    let r = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            org::audit_in(
                tx,
                &auditor,
                &org_id,
                ActorKind::ApiKey,
                Some(&key_id),
                "api_key.use",
                Some(&key_id),
                json!({ "method": method, "route": route, "status": status }),
                Some(&ip),
                now,
            )?;
            tx.execute(
                "UPDATE org_api_keys SET last_used_at = ?2
                  WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at < ?2 - 60)",
                params![key_id, now as i64],
            )?;
            Ok(())
        })
        .await;
    if r.is_err() {
        tracing::error!("org API use could not be audited");
    }
}

// ---- router ------------------------------------------------------------------------------------

pub(crate) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/org/v1/key", get(key_get).delete(key_delete))
        .route("/org/v1/org", get(org_get))
        .route("/org/v1/members", get(members_list).post(member_invite))
        .route(
            "/org/v1/members/{member}",
            get(member_get).patch(member_patch).delete(member_remove),
        )
        .route("/org/v1/members/{member}/suspend", post(member_suspend))
        .route(
            "/org/v1/members/{member}/reactivate",
            post(member_reactivate),
        )
        .route(
            "/org/v1/members/{member}/resend-invite",
            post(member_resend),
        )
        .route("/org/v1/roles", get(roles_list))
        .route("/org/v1/agents", get(agents_list).post(agent_create))
        .route(
            "/org/v1/agents/{agent}",
            get(agent_get).delete(agent_delete),
        )
        .route("/org/v1/agents/{agent}/disconnect", post(agent_disconnect))
        .route("/org/v1/agents/{agent}/delegations", get(agent_delegations))
        .route("/org/v1/agents/{agent}/enrollments", get(agent_enrollments))
        .route(
            "/org/v1/agents/{agent}/enrollments/{enrollment}",
            axum::routing::delete(agent_enrollment_revoke),
        )
        .route("/org/v1/rules", get(rules_list).post(rule_create))
        .route("/org/v1/rules/evaluate", post(rule_evaluate))
        .route(
            "/org/v1/rules/{rule}",
            axum::routing::patch(rule_patch).delete(rule_delete),
        )
        .route("/org/v1/rooms", get(rooms_list).post(room_create))
        .route(
            "/org/v1/rooms/{room}",
            axum::routing::patch(room_patch).delete(room_delete),
        )
        .route("/org/v1/rooms/{room}/members", post(room_invite))
        .route("/org/v1/audit", get(audit_list))
        .route("/org/v1/usage", get(usage))
        .route("/org/v1/billing", get(billing))
        .route("/org/v1/webhooks", get(webhooks_list).post(webhook_create))
        .route(
            "/org/v1/webhooks/{webhook}",
            axum::routing::delete(webhook_delete),
        )
        .route("/org/v1/webhooks/{webhook}/rotate", post(webhook_rotate))
        .route(
            "/org/v1/webhooks/{webhook}/deliveries",
            get(webhook_deliveries),
        )
        .route("/org/v1/messages", post(message_send))
        .route_layer(axum::middleware::from_fn_with_state(state, gate))
}

type Body = Result<Json<Value>, JsonRejection>;
type Key = Extension<Arc<KeyCtx>>;

fn ip_of(cip: &ClientIp) -> Option<String> {
    Some(cip.as_str().to_string())
}

/// Audit a change made by a key, inside the change's transaction.
#[allow(clippy::too_many_arguments)]
fn audit_by_key(
    tx: &Connection,
    state_auditor: &crate::audit::Auditor,
    ctx: &KeyCtx,
    action: &str,
    target: Option<&str>,
    details: Value,
    ip: Option<&str>,
    now: u64,
) -> GResult<()> {
    org::audit_in(
        tx,
        state_auditor,
        &ctx.org.id,
        ActorKind::ApiKey,
        Some(&ctx.key_id),
        action,
        target,
        details,
        ip,
        now,
    )
}

// ---- the key itself ----------------------------------------------------------------------------

async fn key_get(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    respond(&state, Ok(ApiOut::data(ctx.json())))
}

/// `DELETE /key` — a key may always revoke itself (a leaked key's fastest way out).
async fn key_delete(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(cip): Extension<ClientIp>,
) -> Response {
    let auditor = state.auditor.clone();
    let ip = ip_of(&cip);
    let c2 = ctx.clone();
    let result = state
        .store
        .group_tx(move |tx| -> GResult<ApiOut> {
            let now = now_unix();
            org_api_keys::revoke_in(tx, &c2.org.id, &c2.key_id, &c2.key_id, now)?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "api_key.revoke",
                Some(&c2.key_id),
                json!({ "self": true }),
                ip.as_deref(),
                now,
            )?;
            Ok(ApiOut::data(json!({ "id": c2.key_id, "revoked": true })))
        })
        .await;
    respond(&state, result)
}

// ---- organization ------------------------------------------------------------------------------

fn org_api_json(c: &Connection, o: &org::OrgRow, now: u64) -> GResult<Value> {
    Ok(json!({
        "id": o.id,
        "name": o.name,
        "namespace": o.namespace,
        "status": o.status,
        "seats": o.seats,
        "seats_used": org::seats_used(c, &o.id, now)?,
        "agent_limit": o.agent_limit,
        "agents_used": org::agents_used(c, &o.id)?,
        "storage_bytes": o.storage_bytes,
        "plan_code": o.plan_code,
        "period_end": iso(o.period_end),
        "settings": o.settings,
        "created_at": iso(Some(o.created_at)),
    }))
}

async fn org_get(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let o = load_org(c, &id)?.ok_or_else(|| not_found("organization"))?;
                Ok(ApiOut::data(org_api_json(c, &o, now_unix())?))
            })
            .await
    }
    .await;
    respond(&state, result)
}

// ---- members -----------------------------------------------------------------------------------

fn member_api(c: &Connection, m: &MemberRow) -> GResult<Value> {
    let roles = roles_of(c, &m.org_id, &m.id)?;
    Ok(json!({
        "id": m.id,
        "handle": m.handle,
        "address": m.identity,
        "first_name": m.first_name,
        "last_name": m.last_name,
        "status": m.status,
        "roles": roles.names(),
        "contact_masked": m.contact_masked,
        "invited_at": iso(m.invited_at),
        "joined_at": iso(m.joined_at),
        "last_active_at": iso(m.last_active_at),
    }))
}

/// `mem_…` or the name inside the org (`john` for `/acme/john`). Removed members are gone.
fn resolve_member(c: &Connection, o: &org::OrgRow, reference: &str) -> GResult<MemberRow> {
    let r = reference.trim();
    let found = if r.starts_with("mem_") {
        load_member(c, &o.id, r)?
    } else {
        let name = r
            .trim_start_matches('/')
            .trim_start_matches(&format!("{}/", o.namespace));
        let handle = format!("/{}/{}", o.namespace, name.to_ascii_lowercase());
        c.query_row(
            &format!(
                "SELECT {} FROM org_members WHERE org_id = ?1 AND handle = ?2 AND status <> 'removed'",
                org::MEMBER_COLS
            ),
            params![o.id, handle],
            org::map_member,
        )
        .optional()?
    };
    found
        .filter(|m| m.status != "removed")
        .ok_or_else(|| not_found("member"))
}

/// A key manages ordinary members only: never the owner or anyone holding an admin role (§18.2).
fn require_ordinary(c: &Connection, m: &MemberRow) -> GResult<()> {
    // A pending invitation's roles count too: they become real on acceptance.
    let pending: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_invites WHERE member_id = ?1 AND used_at IS NULL
            AND revoked_at IS NULL AND roles NOT IN ('[]', ''))",
        [&m.id],
        |r| r.get(0),
    )?;
    if roles_of(c, &m.org_id, &m.id)?.0.is_empty() && !pending {
        Ok(())
    } else {
        Err(GErr::forbidden(
            "forbidden",
            "API keys manage members without an admin role only; use the dashboard for this person",
        ))
    }
}

async fn members_list(
    State(state): State<AppState>,
    Extension(ctx): Key,
    q: Result<Query<Page>, QueryRejection>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let p = page(q)?;
        let n = limit(&p)?;
        let after = decode_cursor(&p)?.unwrap_or_default();
        if let Some(s) = &p.status {
            if !matches!(s.as_str(), "invited" | "active" | "suspended") {
                return Err(bad("bad_request", "status is invited, active or suspended"));
            }
        }
        let (id, status) = (ctx.org.id.clone(), p.status.clone());
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {} FROM org_members WHERE org_id = ?1 AND status <> 'removed'
                        AND (?2 IS NULL OR status = ?2) AND id > ?3 ORDER BY id LIMIT ?4",
                    org::MEMBER_COLS
                ))?;
                let rows: Vec<MemberRow> = stmt
                    .query_map(params![id, status, after, n + 1], org::map_member)?
                    .collect::<Result<_, _>>()?;
                let rows = rows
                    .iter()
                    .map(|m| Ok((m.id.clone(), member_api(c, m)?)))
                    .collect::<GResult<Vec<_>>>()?;
                let (data, next) = paginate(rows, n);
                Ok(ApiOut::list(data, next))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn member_get(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Path(member): Path<String>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let o = ctx.org.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let m = resolve_member(c, &o, &member)?;
                Ok(ApiOut::data(member_api(c, &m)?))
            })
            .await
    }
    .await;
    respond(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteReq {
    first_name: String,
    last_name: String,
    contact: String,
    handle: String,
    /// Refused when present: role grants are human-only.
    #[serde(default)]
    roles: Option<Value>,
}

async fn member_invite(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("members:write", &[Perm::ManagePeople])?;
        let (req, _): (InviteReq, String) = parse(body)?;
        if req
            .roles
            .as_ref()
            .is_some_and(|r| !r.is_null() && r != &json!([]))
        {
            return Err(human_required("Granting a role"));
        }
        ctx.require_writable()?;
        let row = crate::org_routes::CheckedInvite {
            first: crate::org_routes::text("first_name", &req.first_name, 80)?,
            last: crate::org_routes::text("last_name", &req.last_name, 80)?,
            contact: org::canonical_contact(&req.contact)
                .ok_or_else(|| bad("invalid_contact", "contact must be a valid contact address"))?,
            handle: org::org_handle(&ctx.org, &req.handle)?,
            roles: org::Roles::default(),
        };
        let id = ctx.org.id.clone();
        rate_limit(&format!("orginv:{id}"), 1_000, 86_400)?;
        let deliver = crate::filtering::feature_on(
            &state,
            Some(&ctx.org.owner_account),
            capabilities::ORG_INVITE_EMAIL,
        )
        .await;
        let (vault, auditor) = (state.vault.clone(), state.auditor.clone());
        let ip = ip_of(&cip);
        let c2 = ctx.clone();
        write(&state, &idem, move |tx, now| {
            let o = load_org(tx, &id)?.ok_or_else(|| not_found("organization"))?;
            let (mid, inv, link) = match crate::org_routes::invite_one(
                tx, &vault, &id, &o, &c2.key_id, &row, deliver, now,
            )? {
                Ok(done) => done,
                Err(code) => {
                    let message = match code {
                        "seat_limit" => "every seat is in use",
                        "domain_not_verified" => {
                            "the contact's domain is not a verified domain of this organization"
                        }
                        "already_invited" => {
                            "this person is already a member or has a live invitation"
                        }
                        "handle_taken" => "that address is taken",
                        _ => "the invitation could not be created",
                    };
                    return Err(GErr::conflict(code, message));
                }
            };
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "member.invite",
                Some(&mid),
                json!({ "handle": row.handle, "roles": Vec::<String>::new(), "invite": inv }),
                ip.as_deref(),
                now,
            )?;
            let m = load_member(tx, &id, &mid)?.ok_or_else(|| not_found("member"))?;
            Ok(ApiOut::created(json!({
                "member": member_api(tx, &m)?,
                "invitation": {
                    "id": inv,
                    "link": link,
                    "expires_at": iso(Some(now as i64 + org::INVITE_TTL_SECS as i64)),
                    "delivery": if deliver { "queued" } else { "copy_link" },
                },
            }))
            .secret())
        })
        .await
    }
    .await;
    respond(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberPatch {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    roles: Option<Value>,
    #[serde(default)]
    status: Option<Value>,
}

async fn member_patch(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(member): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("members:write", &[Perm::ManagePeople])?;
        let (req, _): (MemberPatch, String) = parse(body)?;
        if req.roles.is_some() {
            return Err(human_required("Changing roles"));
        }
        if req.status.is_some() {
            return Err(bad("bad_request", "use POST /members/{member}/suspend or /reactivate"));
        }
        let first = req.first_name.as_deref().map(|v| crate::org_routes::text("first_name", v, 80)).transpose()?;
        let last = req.last_name.as_deref().map(|v| crate::org_routes::text("last_name", v, 80)).transpose()?;
        if first.is_none() && last.is_none() {
            return Err(bad("bad_request", "nothing to change"));
        }
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let m = resolve_member(tx, &c2.org, &member)?;
            require_ordinary(tx, &m)?;
            tx.execute(
                "UPDATE org_members SET first_name = COALESCE(?2, first_name), last_name = COALESCE(?3, last_name) WHERE id = ?1",
                params![m.id, first, last],
            )?;
            audit_by_key(tx, &auditor, &c2, "member.rename", Some(&m.id), json!({}), ip.as_deref(), now)?;
            let m = load_member(tx, &c2.org.id, &m.id)?.ok_or_else(|| not_found("member"))?;
            Ok(ApiOut::data(member_api(tx, &m)?))
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn member_remove(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(member): Path<String>,
    q: Result<Query<Page>, QueryRejection>,
) -> Response {
    let result = async {
        ctx.require("members:write", &[Perm::ManagePeople])?;
        let p = page(q)?;
        let mode = p.mode.unwrap_or_else(|| "reserve".into());
        if !matches!(mode.as_str(), "reserve" | "delete") {
            return Err(bad("bad_request", "mode is reserve or delete"));
        }
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        let internal = ctx.org.internal_account.clone().unwrap_or_default();
        write(&state, &idem, move |tx, now| {
            let m = resolve_member(tx, &c2.org, &member)?;
            require_ordinary(tx, &m)?;
            let mut out = crate::groups::Outcome::default();
            crate::org_routes::offboard_in(
                tx, &c2.org.id, &m, &internal, &mode, &c2.key_id, now, &mut out,
            )?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "member.remove",
                Some(&m.id),
                json!({ "mode": mode, "handle": m.handle }),
                ip.as_deref(),
                now,
            )?;
            let mut api = ApiOut::data(json!({
                "id": m.id, "removed": true, "mode": mode,
                "handle_held": mode == "reserve" && m.handle.is_some(),
            }));
            api.wake = out.wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

/// Suspend or reactivate an ordinary member, exactly as the dashboard does.
async fn member_status(
    state: AppState,
    ctx: Arc<KeyCtx>,
    idem: Idem,
    cip: ClientIp,
    member: String,
    to: &'static str,
) -> Response {
    let result = async {
        ctx.require("members:write", &[Perm::ManagePeople])?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let m = resolve_member(tx, &c2.org, &member)?;
            require_ordinary(tx, &m)?;
            if m.status == "invited" {
                return Err(GErr::conflict(
                    "invite_pending",
                    "they have not joined yet; remove the member to withdraw the invitation",
                ));
            }
            let mut api = ApiOut::default();
            if m.status != to {
                tx.execute(
                    "UPDATE org_members SET status = ?2, authority_revision = authority_revision + 1 WHERE id = ?1",
                    params![m.id, to],
                )?;
                if to == "suspended" {
                    crate::org_agents::revoke_member_grants_in(tx, &c2.org.id, &m.id, now)?;
                }
                audit_by_key(tx, &auditor, &c2,
                    if to == "suspended" { "member.suspend" } else { "member.reinstate" },
                    Some(&m.id), json!({}), ip.as_deref(), now)?;
                if let Some(a) = &m.account_id {
                    org::emit_to(tx, a, "org.member", &json!({
                        "org_id": c2.org.id, "member_id": m.id, "status": to,
                        "roles": roles_of(tx, &c2.org.id, &m.id)?.names(),
                    }), now)?;
                    api.wake.push(a.clone());
                }
            }
            let m = load_member(tx, &c2.org.id, &m.id)?.ok_or_else(|| not_found("member"))?;
            let mut out = ApiOut::data(member_api(tx, &m)?);
            out.wake = api.wake;
            Ok(out)
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn member_suspend(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(member): Path<String>,
) -> Response {
    member_status(state, ctx, idem, cip, member, "suspended").await
}

async fn member_reactivate(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(member): Path<String>,
) -> Response {
    member_status(state, ctx, idem, cip, member, "active").await
}

async fn member_resend(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(member): Path<String>,
) -> Response {
    let result = async {
        ctx.require("members:write", &[Perm::ManagePeople])?;
        ctx.require_writable()?;
        let deliver = crate::filtering::feature_on(
            &state,
            Some(&ctx.org.owner_account),
            capabilities::ORG_INVITE_EMAIL,
        )
        .await;
        let (vault, auditor, ip, c2) = (state.vault.clone(), state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let m = resolve_member(tx, &c2.org, &member)?;
            if m.status != "invited" {
                return Err(GErr::conflict("invite_used", "this member has already joined"));
            }
            rate_limit(&format!("orgresend:{}", m.id), 5, 3_600)?;
            let prior: Option<(String, String, String, String)> = tx
                .query_row(
                    "SELECT id, handle, contact_hash, roles FROM org_invites WHERE member_id = ?1 AND used_at IS NULL
                      ORDER BY created_at DESC LIMIT 1",
                    [&m.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((old, handle, contact_hash, roles)) = prior else {
                return Err(not_found("invitation"));
            };
            let roles: Vec<String> = serde_json::from_str(&roles).unwrap_or_default();
            if !roles.is_empty() {
                return Err(GErr::forbidden(
                    "forbidden",
                    "this invitation grants a role; resend it from the dashboard",
                ));
            }
            tx.execute(
                "UPDATE org_invites SET revoked_at = ?2 WHERE member_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
                params![m.id, now as i64],
            )?;
            let o = load_org(tx, &c2.org.id)?.ok_or_else(|| not_found("organization"))?;
            if org::seats_used(tx, &o.id, now)? >= o.seats {
                return Err(GErr::conflict("seat_limit", "every seat is in use"));
            }
            let (inv, link) = crate::org_routes::new_invite_in(
                tx, &vault, &o.id, &m.id, &handle, &contact_hash, &org::Roles::default(), &c2.key_id, deliver, now,
            )?;
            audit_by_key(tx, &auditor, &c2, "member.invite_resend", Some(&m.id),
                json!({ "invite": inv, "replaces": old }), ip.as_deref(), now)?;
            Ok(ApiOut::data(json!({
                "member": member_api(tx, &m)?,
                "invitation": {
                    "id": inv,
                    "link": link,
                    "expires_at": iso(Some(now as i64 + org::INVITE_TTL_SECS as i64)),
                    "delivery": if deliver { "queued" } else { "copy_link" },
                },
            }))
            .secret())
        })
        .await
    }
    .await;
    respond(&state, result)
}

// ---- roles -------------------------------------------------------------------------------------

fn role_description(r: Role) -> &'static str {
    match r {
        Role::Owner => "Everything, including billing, deleting the organization and transferring ownership",
        Role::Admin => "Everything except deleting the organization, transferring ownership and appointing admins",
        Role::PeopleAdmin => "Invite, suspend and remove members; manage rooms",
        Role::AgentAdmin => "Create and configure agents, enrollment codes and delegations",
        Role::BillingAdmin => "View and change the plan and payment method",
    }
}

async fn roles_list(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let mut out = Vec::new();
                for role in Role::ALL {
                    let mut stmt = c.prepare(
                        "SELECT m.id, m.handle FROM org_roles r JOIN org_members m ON m.id = r.member_id
                          WHERE r.org_id = ?1 AND r.role = ?2 AND m.status <> 'removed' ORDER BY m.id",
                    )?;
                    let members: Vec<Value> = stmt
                        .query_map(params![id, role.as_str()], |r| {
                            Ok(json!({ "id": r.get::<_, String>(0)?, "handle": r.get::<_, Option<String>>(1)? }))
                        })?
                        .collect::<Result<_, _>>()?;
                    out.push(json!({
                        "role": role.as_str(),
                        "description": role_description(role),
                        "members": members,
                    }));
                }
                Ok(ApiOut::list(out, None))
            })
            .await
    }
    .await;
    respond(&state, result)
}

// ---- agents ------------------------------------------------------------------------------------

fn agent_api(c: &Connection, org_id: &str, identity: &str, now: u64) -> GResult<Value> {
    let mut v = crate::org_agents::agent_json(c, org_id, identity, now)?;
    for k in ["created_at", "last_seen_at"] {
        v[k] = iso(v[k].as_i64());
    }
    Ok(v)
}

/// An agent by name (`ci-bot`), handle or mailbox address.
fn resolve_agent(c: &Connection, o: &org::OrgRow, reference: &str) -> GResult<String> {
    let r = reference.trim();
    let r = if r.starts_with('/') || r.contains('/') {
        r.to_string()
    } else {
        format!("/{}/{}", o.namespace, r.to_ascii_lowercase())
    };
    crate::org_agents::agent_in_org(c, &o.id, &r).map_err(|_| not_found("agent"))
}

async fn agents_list(
    State(state): State<AppState>,
    Extension(ctx): Key,
    q: Result<Query<Page>, QueryRejection>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let p = page(q)?;
        let n = limit(&p)?;
        let after = decode_cursor(&p)?.unwrap_or_default();
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let mut stmt = c.prepare(
                    "SELECT identity FROM org_agents WHERE org_id = ?1 AND status <> 'deleted' AND identity > ?2
                      ORDER BY identity LIMIT ?3",
                )?;
                let ids: Vec<String> = stmt
                    .query_map(params![id, after, n + 1], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let now = now_unix();
                let rows = ids
                    .iter()
                    .map(|a| Ok((a.clone(), agent_api(c, &id, a, now)?)))
                    .collect::<GResult<Vec<_>>>()?;
                let (data, next) = paginate(rows, n);
                Ok(ApiOut::list(data, next))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn agent_get(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Path(agent): Path<String>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let o = ctx.org.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let a = resolve_agent(c, &o, &agent)?;
                Ok(ApiOut::data(agent_api(c, &o.id, &a, now_unix())?))
            })
            .await
    }
    .await;
    respond(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentReq {
    handle: String,
    #[serde(default)]
    label: Option<String>,
    /// Refused when present: delegations are granted by people.
    #[serde(default)]
    operators: Option<Value>,
}

async fn agent_create(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("agents:write", &[Perm::ManageAgents])?;
        let (req, _): (AgentReq, String) = parse(body)?;
        if req
            .operators
            .as_ref()
            .is_some_and(|o| !o.is_null() && o != &json!([]))
        {
            return Err(human_required("Granting delegations"));
        }
        ctx.require_writable()?;
        let handle = org::org_handle(&ctx.org, &req.handle)?;
        let label = crate::org_agents::label_ok(req.label.as_deref())?;
        rate_limit(&format!("orgagent:{}", ctx.org.id), 500, 86_400)?;
        let (vault, auditor, ip, c2) = (
            state.vault.clone(),
            state.auditor.clone(),
            ip_of(&cip),
            ctx.clone(),
        );
        write(&state, &idem, move |tx, now| {
            let o = load_org(tx, &c2.org.id)?.ok_or_else(|| not_found("organization"))?;
            let address = crate::org_agents::create_agent_in(
                tx,
                &vault,
                &o,
                &handle,
                label.as_deref(),
                &c2.key_id,
                now,
            )?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "agent.create",
                Some(&address),
                json!({ "handle": handle, "operators": 0 }),
                ip.as_deref(),
                now,
            )?;
            Ok(ApiOut::created(agent_api(tx, &o.id, &address, now)?))
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn agent_delete(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(agent): Path<String>,
) -> Response {
    let result = async {
        ctx.require("agents:write", &[Perm::ManageAgents])?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let id = c2.org.id.clone();
            let identity = resolve_agent(tx, &c2.org, &agent)?;
            crate::org_agents::revoke_all_devices_in(tx, &id, Some(&identity), now)?;
            tx.execute(
                "UPDATE org_delegations SET revoked_at = ?3, revision = revision + 1
                  WHERE org_id = ?1 AND target_identity = ?2 AND revoked_at IS NULL",
                params![id, identity, now as i64],
            )?;
            let mut out = crate::groups::Outcome::default();
            crate::org_rooms::remove_everywhere_in(tx, &identity, &identity, now, &mut out)?;
            tx.execute(
                "UPDATE org_agents SET status = 'deleted', deleted_at = ?3 WHERE org_id = ?1 AND identity = ?2",
                params![id, identity, now as i64],
            )?;
            tx.execute("UPDATE identities SET handle = NULL WHERE address = ?1", [&identity])?;
            audit_by_key(tx, &auditor, &c2, "agent.delete", Some(&identity), json!({}), ip.as_deref(), now)?;
            let mut api = ApiOut::data(json!({ "id": identity, "deleted": true }));
            api.wake = out.wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn agent_disconnect(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(agent): Path<String>,
) -> Response {
    let result = async {
        ctx.require("agents:write", &[Perm::ManageAgents])?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let identity = resolve_agent(tx, &c2.org, &agent)?;
            crate::org_agents::revoke_all_devices_in(tx, &c2.org.id, Some(&identity), now)?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "agent.disconnect",
                Some(&identity),
                json!({}),
                ip.as_deref(),
                now,
            )?;
            Ok(ApiOut::data(agent_api(tx, &c2.org.id, &identity, now)?))
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn agent_delegations(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Path(agent): Path<String>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let o = ctx.org.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let identity = resolve_agent(c, &o, &agent)?;
                let mut stmt = c.prepare(
                    "SELECT d.id, d.grantee_member_id, m.handle, d.capabilities, d.expires_at, d.granted_at
                       FROM org_delegations d JOIN org_members m ON m.id = d.grantee_member_id
                      WHERE d.org_id = ?1 AND d.target_identity = ?2 AND d.revoked_at IS NULL
                        AND (d.expires_at IS NULL OR d.expires_at > ?3)
                      ORDER BY d.granted_at, d.id",
                )?;
                let rows: Vec<Value> = stmt
                    .query_map(params![o.id, identity, now_unix() as i64], |r| {
                        let caps: String = r.get(3)?;
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "member_id": r.get::<_, String>(1)?,
                            "member_handle": r.get::<_, Option<String>>(2)?,
                            "capabilities": serde_json::from_str::<Value>(&caps).unwrap_or(Value::Null),
                            "expires_at": iso(r.get::<_, Option<i64>>(4)?),
                            "granted_at": iso(r.get::<_, Option<i64>>(5)?),
                        }))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(ApiOut::list(rows, None))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn agent_enrollments(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Path(agent): Path<String>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let o = ctx.org.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let identity = resolve_agent(c, &o, &agent)?;
                // Metadata only: the code itself is never stored, let alone shown.
                let mut stmt = c.prepare(
                    "SELECT e.id, e.created_at, e.expires_at, e.redeemed_at, e.revoked_at, m.handle
                       FROM enrollments e LEFT JOIN org_members m ON m.id = e.created_by
                      WHERE e.org_id = ?1 AND e.identity = ?2 ORDER BY e.created_at DESC, e.id LIMIT 200",
                )?;
                let rows: Vec<Value> = stmt
                    .query_map(params![o.id, identity], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "created_at": iso(Some(r.get::<_, i64>(1)?)),
                            "expires_at": iso(Some(r.get::<_, i64>(2)?)),
                            "redeemed_at": iso(r.get::<_, Option<i64>>(3)?),
                            "revoked_at": iso(r.get::<_, Option<i64>>(4)?),
                            "created_by": r.get::<_, Option<String>>(5)?,
                        }))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(ApiOut::list(rows, None))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn agent_enrollment_revoke(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path((agent, enrollment)): Path<(String, String)>,
) -> Response {
    let result = async {
        ctx.require("agents:write", &[Perm::ManageAgents])?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let identity = resolve_agent(tx, &c2.org, &agent)?;
            let grant: Option<String> = tx
                .query_row(
                    "SELECT grant_id FROM enrollments WHERE id = ?1 AND org_id = ?2 AND identity = ?3
                        AND redeemed_at IS NULL AND revoked_at IS NULL",
                    params![enrollment, c2.org.id, identity],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(grant) = grant else {
                return Err(not_found("unredeemed enrollment code"));
            };
            tx.execute("UPDATE enrollments SET revoked_at = ?2 WHERE id = ?1", params![enrollment, now as i64])?;
            // Its grant was made for this code only; nothing else hangs off an unredeemed one.
            tx.execute(
                "UPDATE device_grants SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL
                    AND NOT EXISTS (SELECT 1 FROM device_credentials WHERE parent_grant_id = ?1)",
                params![grant, now as i64],
            )?;
            audit_by_key(tx, &auditor, &c2, "agent.enrollment_revoke", Some(&identity),
                json!({ "enrollment": enrollment }), ip.as_deref(), now)?;
            Ok(ApiOut::data(json!({ "id": enrollment, "revoked": true })))
        })
        .await
    }
    .await;
    respond(&state, result)
}

// ---- rules -------------------------------------------------------------------------------------

fn rule_api(c: &Connection, id: &str) -> GResult<Value> {
    let mut v = crate::org_rules::rule_json(c, id)?;
    for k in ["created_at", "updated_at"] {
        v[k] = iso(v[k].as_i64());
    }
    Ok(v)
}

/// A rule only restricts: it blocks or holds for review and grants nothing (SDS §18.1: trust-raising
/// rule writes need a person).
fn restrictive(d: &crate::org_rules::Draft) -> bool {
    matches!(d.admission.as_deref(), None | Some("block"))
        && matches!(d.autonomy.as_deref(), None | Some("review"))
        && d.verbs.is_none()
        && !d.known
        && !d.locked
}

/// A rule only grants: removing it can only lower trust.
fn purely_granting(d: &crate::org_rules::Draft) -> bool {
    d.admission.as_deref() != Some("block") && d.autonomy.as_deref() != Some("review")
}

fn made_by_key(c: &Connection, org_id: &str, rule: &str) -> GResult<bool> {
    let by: Option<Option<String>> = c
        .query_row(
            "SELECT created_by FROM org_rules WHERE org_id = ?1 AND id = ?2",
            params![org_id, rule],
            |r| r.get(0),
        )
        .optional()?;
    match by {
        None => Err(not_found("rule")),
        Some(by) => Ok(by.is_some_and(|b| b.starts_with("okey_"))),
    }
}

/// The creator's authority to edit rules for these owners (§6.4: members' or agents' rules).
fn rule_editor(ctx: &KeyCtx, owners: &crate::org_rules::Pat) -> GResult<()> {
    if crate::org_rules::editor_may(&ctx.creator_roles, owners) {
        Ok(())
    } else {
        Err(GErr::forbidden(
            "forbidden",
            "the person who created this key cannot edit rules for these mailboxes",
        ))
    }
}

async fn rules_list(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let mut stmt = c.prepare(
                    "SELECT id FROM org_rules WHERE org_id = ?1 ORDER BY priority, created_at, id",
                )?;
                let ids: Vec<String> = stmt
                    .query_map([&id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let rows = ids
                    .iter()
                    .map(|r| rule_api(c, r))
                    .collect::<GResult<Vec<_>>>()?;
                Ok(ApiOut::list(rows, None))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn rule_create(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("rules:write", &[Perm::EditMemberRules, Perm::EditAgentRules])?;
        let (req, _): (crate::org_rules::RuleBody, String) = parse(body)?;
        if req.owners.is_none() || req.peers.is_none() {
            return Err(bad("invalid_rule", "owners and peers are required"));
        }
        let mut d = crate::org_rules::Draft {
            owners: String::new(),
            peers: String::new(),
            admission: None,
            autonomy: None,
            verbs: None,
            known: false,
            locked: false,
            priority: 100,
            description: None,
        };
        crate::org_rules::apply(&mut d, req);
        let (owners, _) = crate::org_rules::validate(&d, &ctx.org.namespace)?;
        if !restrictive(&d) {
            return Err(human_required("A rule that allows, marks known, automates or locks"));
        }
        rule_editor(&ctx, &owners)?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let rid = crate::org_rules::insert(tx, &c2.org.id, &d, &c2.key_id, now)?;
            let mut out = crate::groups::Outcome::default();
            crate::org_routes::policy_changed(tx, &c2.org.id, now, &mut out)?;
            audit_by_key(tx, &auditor, &c2, "rule.create", Some(&rid),
                json!({ "owners": d.owners, "peers": d.peers, "admission": d.admission, "autonomy": d.autonomy,
                        "priority": d.priority }), ip.as_deref(), now)?;
            let mut api = ApiOut::created(rule_api(tx, &rid)?);
            api.wake = out.wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn rule_patch(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(rule): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("rules:write", &[Perm::EditMemberRules, Perm::EditAgentRules])?;
        let (req, _): (crate::org_rules::RuleBody, String) = parse(body)?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            if !made_by_key(tx, &c2.org.id, &rule)? {
                return Err(human_required("Changing a rule a person made"));
            }
            let mut d = crate::org_rules::load_draft(tx, &c2.org.id, &rule)?.ok_or_else(|| not_found("rule"))?;
            crate::org_rules::apply(&mut d, req);
            let (owners, _) = crate::org_rules::validate(&d, &c2.org.namespace)?;
            if !restrictive(&d) {
                return Err(human_required("A rule that allows, marks known, automates or locks"));
            }
            rule_editor(&c2, &owners)?;
            crate::org_rules::update(tx, &rule, &d, now)?;
            let mut out = crate::groups::Outcome::default();
            crate::org_routes::policy_changed(tx, &c2.org.id, now, &mut out)?;
            audit_by_key(tx, &auditor, &c2, "rule.update", Some(&rule),
                json!({ "owners": d.owners, "peers": d.peers, "admission": d.admission, "autonomy": d.autonomy,
                        "priority": d.priority }), ip.as_deref(), now)?;
            let mut api = ApiOut::data(rule_api(tx, &rule)?);
            api.wake = out.wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn rule_delete(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(rule): Path<String>,
) -> Response {
    let result = async {
        ctx.require(
            "rules:write",
            &[Perm::EditMemberRules, Perm::EditAgentRules],
        )?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let by_key = made_by_key(tx, &c2.org.id, &rule)?;
            let d = crate::org_rules::load_draft(tx, &c2.org.id, &rule)?
                .ok_or_else(|| not_found("rule"))?;
            // Removing a person's restriction would raise trust; removing a grant only lowers it.
            if !by_key && !purely_granting(&d) {
                return Err(human_required("Removing a restriction a person made"));
            }
            let owners = crate::org_rules::parse_pat(&d.owners, &c2.org.namespace, true)
                .map_err(|m| bad("invalid_rule", m))?;
            rule_editor(&c2, &owners)?;
            tx.execute(
                "DELETE FROM org_rules WHERE id = ?1 AND org_id = ?2",
                params![rule, c2.org.id],
            )?;
            let mut out = crate::groups::Outcome::default();
            crate::org_routes::policy_changed(tx, &c2.org.id, now, &mut out)?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "rule.delete",
                Some(&rule),
                json!({ "owners": d.owners, "peers": d.peers }),
                ip.as_deref(),
                now,
            )?;
            let mut api = ApiOut::data(json!({ "id": rule, "deleted": true }));
            api.wake = out.wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn rule_evaluate(State(state): State<AppState>, Extension(ctx): Key, body: Body) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let (req, _): (crate::org_routes::EvalReq, String) = parse(body)?;
        let v = crate::org_routes::evaluate_in(
            &state,
            &ctx.org.id,
            &req.from,
            &req.to,
            req.verb.as_deref(),
        )
        .await?;
        Ok(ApiOut::data(v))
    }
    .await;
    respond(&state, result)
}

// ---- rooms -------------------------------------------------------------------------------------

async fn rooms_list(
    State(state): State<AppState>,
    Extension(ctx): Key,
    q: Result<Query<Page>, QueryRejection>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let p = page(q)?;
        let n = limit(&p)?;
        let after = decode_cursor(&p)?.unwrap_or_default();
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let mut stmt = c.prepare(
                    "SELECT id FROM groups WHERE org_id = ?1 AND deleted_at IS NULL AND id > ?2 ORDER BY id LIMIT ?3",
                )?;
                let ids: Vec<String> = stmt
                    .query_map(params![id, after, n + 1], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let rows = ids
                    .iter()
                    .map(|g| Ok((g.clone(), crate::org_rooms::room_json(c, g)?)))
                    .collect::<GResult<Vec<_>>>()?;
                let (data, next) = paginate(rows, n);
                Ok(ApiOut::list(data, next))
            })
            .await
    }
    .await;
    respond(&state, result)
}

/// A live mailbox of this org (member or agent), by name, handle or address.
fn resolve_org_mailbox(c: &Connection, o: &org::OrgRow, reference: &str) -> GResult<String> {
    let r = reference.trim();
    let handle = if r.starts_with('/') {
        r.to_ascii_lowercase()
    } else {
        format!("/{}/{}", o.namespace, r.to_ascii_lowercase())
    };
    let found: Option<String> = c
        .query_row(
            "SELECT identity FROM org_members WHERE org_id = ?1 AND status = 'active' AND identity IS NOT NULL
                AND (handle = ?2 OR identity = ?3)
             UNION ALL
             SELECT identity FROM org_agents WHERE org_id = ?1 AND status = 'active' AND (handle = ?2 OR identity = ?3)
             LIMIT 1",
            params![o.id, handle, r],
            |row| row.get(0),
        )
        .optional()?;
    found.ok_or_else(|| {
        GErr::bad(
            "not_org_mailbox",
            "that is not an active mailbox of this organization",
        )
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomReq {
    name: String,
    owner: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    join_policy: Option<String>,
    #[serde(default)]
    post_policy: Option<String>,
    #[serde(default)]
    listed: Option<bool>,
    #[serde(default)]
    auto_join: Option<bool>,
}

async fn room_create(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("rooms:write", &[Perm::CreateRoom, Perm::DeleteAnyRoom])?;
        ctx.require_writable()?;
        if !crate::filtering::feature_on(&state, Some(&ctx.org.owner_account), capabilities::GROUPS)
            .await
        {
            return Err(GErr::new(
                StatusCode::NOT_FOUND,
                "feature_disabled",
                "rooms are not enabled for this organization",
            ));
        }
        let (req, _): (RoomReq, String) = parse(body)?;
        let name = crate::org_routes::text("name", &req.name, 80)?;
        let description = req
            .description
            .as_deref()
            .map(|d| crate::org_routes::text("description", d, 500))
            .transpose()?;
        let join_policy = req.join_policy.unwrap_or_else(|| "public".into());
        let post_policy = req.post_policy.unwrap_or_else(|| "everyone".into());
        if !matches!(join_policy.as_str(), "public" | "private")
            || !matches!(post_policy.as_str(), "everyone" | "admins")
        {
            return Err(bad(
                "bad_request",
                "join_policy is public|private, post_policy everyone|admins",
            ));
        }
        let auto_join = req.auto_join.unwrap_or(false);
        if auto_join && join_policy != "public" {
            return Err(bad(
                "bad_request",
                "an auto-join room is public within the organization",
            ));
        }
        let listed = req.listed.unwrap_or(true);
        rate_limit(&format!("orgroomkey:{}", ctx.key_id), 100, 86_400)?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let owner = resolve_org_mailbox(tx, &c2.org, &req.owner)?;
            let owner_account: Option<String> = tx
                .query_row(
                    "SELECT account_id FROM identities WHERE address = ?1",
                    [&owner],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let gid = crate::org_rooms::create_room_in(
                tx,
                &crate::org_rooms::NewRoom {
                    org_id: &c2.org.id,
                    name: &name,
                    description: description.as_deref(),
                    join_policy: &join_policy,
                    post_policy: &post_policy,
                    listed,
                    auto_join,
                    creator: &owner,
                    creator_account: owner_account.as_deref(),
                },
                now,
            )?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "room.create",
                Some(&gid),
                json!({ "auto_join": auto_join, "listed": listed, "owner": owner }),
                ip.as_deref(),
                now,
            )?;
            let mut api = ApiOut::created(crate::org_rooms::room_json(tx, &gid)?);
            if let Some(a) = owner_account {
                api.wake.push(a);
            }
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomPatch {
    #[serde(default)]
    listed: Option<bool>,
    #[serde(default)]
    auto_join: Option<bool>,
}

async fn room_patch(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(room): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("rooms:write", &[Perm::CreateRoom, Perm::DeleteAnyRoom])?;
        let (req, _): (RoomPatch, String) = parse(body)?;
        if req.listed.is_none() && req.auto_join.is_none() {
            return Err(bad("bad_request", "nothing to change"));
        }
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let g = crate::org_routes::room_in_org(tx, &c2.org.id, &room)
                .map_err(|_| not_found("room"))?;
            let mut changed = Vec::new();
            if let Some(a) = req.auto_join {
                if a && g.join_policy != "public" {
                    return Err(bad(
                        "bad_request",
                        "only a public room can join everyone automatically",
                    ));
                }
                tx.execute(
                    "UPDATE groups SET auto_join = ?2 WHERE id = ?1",
                    params![room, a as i64],
                )?;
                changed.push("auto_join");
            }
            if let Some(l) = req.listed {
                tx.execute(
                    "UPDATE groups SET visibility = ?2 WHERE id = ?1",
                    params![room, if l { "listed" } else { "unlisted" }],
                )?;
                changed.push("visibility");
            }
            let wake = crate::org_rooms::settings_changed(tx, &room, &changed, now)?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "room.update",
                Some(&room),
                json!({ "changed": changed }),
                ip.as_deref(),
                now,
            )?;
            let mut api = ApiOut::data(crate::org_rooms::room_json(tx, &room)?);
            api.wake = wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomInviteReq {
    handle: String,
}

/// Invite an org mailbox into a room. The invitation is all the server can do: the person accepts
/// it in their app, and a member's device then adds them to the encryption group (§18.2: no silent
/// key membership).
async fn room_invite(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(room): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("rooms:write", &[Perm::CreateRoom, Perm::DeleteAnyRoom])?;
        ctx.require_writable()?;
        let (req, _): (RoomInviteReq, String) = parse(body)?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let g = crate::org_routes::room_in_org(tx, &c2.org.id, &room)
                .map_err(|_| not_found("room"))?;
            let address = resolve_org_mailbox(tx, &c2.org, &req.handle)?;
            let invitee = crate::groups::Invitee {
                asked: req.handle.clone(),
                address: Some(address.clone()),
                refusal: None,
                filtered: false,
            };
            let mut out = crate::groups::Outcome::default();
            let results = crate::groups::invite_in(tx, &g, &c2.key_id, &[invitee], now, &mut out)?;
            let status = results
                .first()
                .and_then(|r| r["status"].as_str())
                .unwrap_or("invited")
                .to_string();
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "room.invite",
                Some(&room),
                json!({ "invitee": address, "result": status }),
                ip.as_deref(),
                now,
            )?;
            let handle: Option<String> = tx
                .query_row(
                    "SELECT handle FROM identities WHERE address = ?1",
                    [&address],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let mut api = ApiOut::data(json!({
                "room_id": room, "handle": handle, "address": address, "state": status,
            }));
            api.wake = out.wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn room_delete(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(room): Path<String>,
) -> Response {
    let result = async {
        ctx.require("rooms:write", &[Perm::CreateRoom, Perm::DeleteAnyRoom])?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let g = crate::org_routes::room_in_org(tx, &c2.org.id, &room)
                .map_err(|_| not_found("room"))?;
            let wake = crate::groups::delete_in(tx, &g, &c2.key_id, now)?;
            audit_by_key(
                tx,
                &auditor,
                &c2,
                "room.delete",
                Some(&room),
                json!({}),
                ip.as_deref(),
                now,
            )?;
            let mut api = ApiOut::data(json!({ "id": room, "deleted": true }));
            api.wake = wake;
            Ok(api)
        })
        .await
    }
    .await;
    respond(&state, result)
}

// ---- audit, usage, billing ---------------------------------------------------------------------

/// `GET /audit` — oldest first from `since`, for SIEM streaming. Keys appear by their key id.
async fn audit_list(
    State(state): State<AppState>,
    Extension(ctx): Key,
    q: Result<Query<Page>, QueryRejection>,
) -> Response {
    let result = async {
        ctx.require("audit:read", &[Perm::Security])?;
        let p = page(q)?;
        let n = limit(&p)?;
        let after: i64 = match decode_cursor(&p)? {
            Some(k) => k.parse().map_err(|_| bad("invalid_cursor", "this cursor is not one the API returned"))?,
            None => 0,
        };
        let since = match p.since.as_deref() {
            None => 0,
            Some(s) => parse_iso(s).ok_or_else(|| bad("bad_request", "since is an ISO 8601 timestamp"))?,
        };
        let action = p.action.as_deref().map(like_prefix);
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let mut stmt = c.prepare(
                    "SELECT id, at, actor_kind, actor_account, action, target, details FROM audit_log
                      WHERE org_id = ?1 AND id > ?2 AND at >= ?3 AND (?4 IS NULL OR action LIKE ?4 ESCAPE '\\')
                      ORDER BY id LIMIT ?5",
                )?;
                let raw: Vec<(i64, i64, String, Option<String>, String, Option<String>, Option<String>)> = stmt
                    .query_map(params![id, after, since, action, n + 1], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
                    })?
                    .collect::<Result<_, _>>()?;
                let mut rows = Vec::new();
                for (rid, at, kind, actor, action, target, details) in raw {
                    // People by their org handle; keys by key id; account ids stay internal.
                    let actor = match (kind.as_str(), actor) {
                        ("api_key", a) => a,
                        (_, Some(a)) => Some(
                            c.query_row(
                                "SELECT COALESCE(handle, trim(first_name || ' ' || last_name)) FROM org_members
                                  WHERE org_id = ?1 AND account_id = ?2 ORDER BY status = 'removed' LIMIT 1",
                                params![id, a],
                                |r| r.get::<_, Option<String>>(0),
                            )
                            .optional()?
                            .flatten()
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| "system".into()),
                        ),
                        (_, None) => None,
                    };
                    rows.push((
                        rid.to_string(),
                        json!({
                            "id": rid,
                            "at": iso(Some(at)),
                            "actor_kind": kind,
                            "actor": actor,
                            "action": action,
                            "target": target,
                            "details": details.and_then(|d| serde_json::from_str::<Value>(&d).ok()),
                        }),
                    ));
                }
                let (data, next) = paginate(rows, n);
                Ok(ApiOut::list(data, next))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn usage(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let o = ctx.org.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let now = now_unix();
                // The org's mailboxes: live members' and agents'.
                const MAILBOXES: &str = "SELECT identity FROM org_members WHERE org_id = ?1 AND identity IS NOT NULL
                        AND status IN ('active','suspended')
                     UNION SELECT identity FROM org_agents WHERE org_id = ?1 AND status <> 'deleted'";
                let messages: i64 = c.query_row(
                    &format!("SELECT COALESCE(SUM(LENGTH(wrap_blob)), 0) FROM messages
                               WHERE COALESCE(owner, recipient) IN ({MAILBOXES})"),
                    [&o.id],
                    |r| r.get(0),
                )?;
                let files: i64 = c.query_row(
                    &format!("SELECT COALESCE(SUM(bytes), 0) FROM attachments WHERE owner IN ({MAILBOXES})"),
                    [&o.id],
                    |r| r.get(0),
                )?;
                let seconds: i64 = c.query_row(
                    &format!("SELECT COALESCE(SUM(MAX(ended_at - answered_at, 0)), 0) FROM call_log
                               WHERE owner IN ({MAILBOXES}) AND answered_at IS NOT NULL AND ended_at IS NOT NULL
                                 AND started_at >= ?2 AND deleted_at IS NULL"),
                    params![o.id, now as i64 - 30 * 86_400],
                    |r| r.get(0),
                )?;
                let rooms: i64 = c.query_row(
                    "SELECT COUNT(*) FROM groups WHERE org_id = ?1 AND deleted_at IS NULL",
                    [&o.id],
                    |r| r.get(0),
                )?;
                Ok(ApiOut::data(json!({
                    "seats": o.seats,
                    "seats_used": org::seats_used(c, &o.id, now)?,
                    "agent_limit": o.agent_limit,
                    "agents_used": org::agents_used(c, &o.id)?,
                    "rooms": rooms,
                    "storage": { "quota_bytes": o.storage_bytes, "used_bytes": messages + files },
                    "calls": { "minutes_30d": (seconds + 59) / 60 },
                })))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn billing(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::ViewBilling])?;
        let now = now_unix();
        let ents = state
            .store
            .entitlements_for("org", ctx.org.id.clone(), now)
            .await?;
        let best = ents
            .iter()
            .filter(|e| e.active)
            .max_by_key(|e| e.seats.unwrap_or(0))
            .or(ents.first());
        let paid_through = best.and_then(|e| e.paid_through);
        let o = ctx.org.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                Ok(ApiOut::data(json!({
                    "plan_code": o.plan_code,
                    "status": o.status,
                    "seats": o.seats,
                    "seats_used": org::seats_used(c, &o.id, now)?,
                    "agent_limit": o.agent_limit,
                    "agents_used": org::agents_used(c, &o.id)?,
                    "period_end": iso(o.period_end),
                    "paid_through": iso(paid_through),
                    "storage_bytes": o.storage_bytes,
                })))
            })
            .await
    }
    .await;
    respond(&state, result)
}

// ---- webhooks ----------------------------------------------------------------------------------

fn key_by<'a>(ctx: &'a KeyCtx, ip: Option<&'a str>) -> crate::webhooks::By<'a> {
    crate::webhooks::By {
        kind: ActorKind::ApiKey,
        actor: &ctx.key_id,
        label: &ctx.key_id,
        ip,
    }
}

async fn webhooks_list(State(state): State<AppState>, Extension(ctx): Key) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                let rows = crate::webhooks::list_in(c, &id)?;
                Ok(ApiOut::list(
                    rows.iter().map(|h| h.json(true)).collect(),
                    None,
                ))
            })
            .await
    }
    .await;
    respond(&state, result)
}

async fn webhook_create(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        ctx.require("webhooks:write", &[Perm::Security])?;
        rate_limit(&format!("webhookcreate:{}", ctx.org.id), 30, 3_600)?;
        let (req, _): (crate::webhooks::HookReq, String) = parse(body)?;
        let hook = crate::webhooks::validate(&state.webhook_egress, req).await?;
        let (vault, auditor, ip, c2) = (
            state.vault.clone(),
            state.auditor.clone(),
            ip_of(&cip),
            ctx.clone(),
        );
        write(&state, &idem, move |tx, now| {
            let by = key_by(&c2, ip.as_deref());
            let (hid, secret) =
                crate::webhooks::create_in(tx, &vault, &auditor, &c2.org.id, &hook, &by, now)?;
            Ok(ApiOut::created(json!({
                "webhook": crate::webhooks::get_in(tx, &c2.org.id, &hid)?.json(true),
                "secret": secret,
            }))
            .secret())
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn webhook_delete(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(hook): Path<String>,
) -> Response {
    let result = async {
        ctx.require("webhooks:write", &[Perm::Security])?;
        let (auditor, ip, c2) = (state.auditor.clone(), ip_of(&cip), ctx.clone());
        write(&state, &idem, move |tx, now| {
            let by = key_by(&c2, ip.as_deref());
            crate::webhooks::delete_in(tx, &auditor, &c2.org.id, &hook, &by, now)?;
            Ok(ApiOut::data(json!({ "id": hook, "deleted": true })))
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn webhook_rotate(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    Path(hook): Path<String>,
) -> Response {
    let result = async {
        ctx.require("webhooks:write", &[Perm::Security])?;
        rate_limit(&format!("webhookrotate:{hook}"), 20, 3_600)?;
        let (vault, auditor, ip, c2) = (
            state.vault.clone(),
            state.auditor.clone(),
            ip_of(&cip),
            ctx.clone(),
        );
        write(&state, &idem, move |tx, now| {
            let by = key_by(&c2, ip.as_deref());
            let (secret, until) =
                crate::webhooks::rotate_in(tx, &vault, &auditor, &c2.org.id, &hook, &by, now)?;
            Ok(ApiOut::data(json!({
                "webhook": crate::webhooks::get_in(tx, &c2.org.id, &hook)?.json(true),
                "secret": secret,
                "previous_secret_expires_at": iso(Some(until)),
            }))
            .secret())
        })
        .await
    }
    .await;
    respond(&state, result)
}

async fn webhook_deliveries(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Path(hook): Path<String>,
    q: Result<Query<Page>, QueryRejection>,
) -> Response {
    let result = async {
        ctx.require("org:read", &[Perm::View])?;
        let p = page(q)?;
        let n = limit(&p)?.min(100);
        let id = ctx.org.id.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<ApiOut> {
                crate::webhooks::get_in(c, &id, &hook)?;
                Ok(ApiOut::list(
                    crate::webhooks::deliveries_in(c, &hook, n, true)?,
                    None,
                ))
            })
            .await
    }
    .await;
    respond(&state, result)
}

// ---- messages ----------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageReq {
    to: String,
    body: String,
    #[serde(default)]
    thread_title: Option<String>,
}

/// A deterministic `msg_ref` for an idempotent send, so a retry is deduplicated by the send path
/// itself (SDS §14C) even if two retries race.
fn msg_ref_for(key_id: &str, idem_key: &str) -> String {
    const B32: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let h = crate::sha256(format!("org-api-msg-ref\0{key_id}\0{idem_key}").as_bytes());
    let mut out = String::from("mref_");
    let (mut buf, mut bits) = (0u32, 0u32);
    for b in h.iter() {
        buf = (buf << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 && out.len() < 31 {
            bits -= 5;
            out.push(B32[((buf >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// `POST /messages` — a direct message from the key's `send_as` org agent to a mailbox of the same
/// org. Rooms are end-to-end encrypted and the server holds no group keys, so they are refused
/// (`docs/org-api.md` §4).
async fn message_send(
    State(state): State<AppState>,
    Extension(ctx): Key,
    Extension(idem): Extension<Idem>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result: GResult<ApiOut> = async {
        ctx.require("messages:send", &[Perm::ManageAgents])?;
        ctx.require_writable()?;
        let (req, _): (MessageReq, String) = parse(body)?;
        if req.body.trim().is_empty() {
            return Err(bad("bad_request", "body is empty"));
        }
        if req.body.len() > MAX_MESSAGE {
            return Err(GErr::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", "a message body is at most 64 KB"));
        }
        let send_as = ctx.send_as.clone().ok_or_else(|| {
            GErr::conflict("send_as_unavailable", "this key has no sending mailbox")
        })?;
        // A replay answers before anything is sent again.
        if let Some(i) = &idem.0 {
            let (vault, i2) = (state.vault.clone(), i.clone());
            let now = now_unix();
            let prior = state
                .store
                .group_read(move |c: &Connection| replay_in(c, &vault, &i2, now))
                .await?;
            if let Some(replay) = prior {
                return Ok(replay);
            }
        }
        let to = req.to.trim().to_string();
        let o = ctx.org.clone();
        let (sender_ok, room) = {
            let (o2, s2, t2) = (o.clone(), send_as.clone(), to.clone());
            state
                .store
                .group_read(move |c: &Connection| -> GResult<(bool, bool)> {
                    let ok: bool = c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM org_agents WHERE org_id = ?1 AND identity = ?2 AND status = 'active')",
                        params![o2.id, s2],
                        |r| r.get(0),
                    )?;
                    let room: bool = c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM groups WHERE id = ?1)",
                        [&t2],
                        |r| r.get(0),
                    )?;
                    Ok((ok, room))
                })
                .await?
        };
        if room || to.starts_with("grp_") || to.starts_with('#') {
            return Err(GErr::conflict(
                "room_encrypted",
                "rooms are end-to-end encrypted; post with an enrolled agent's own device instead",
            ));
        }
        if !sender_ok || crate::org::mailbox_gate(&state, &send_as).await != crate::org::Gate::Open {
            return Err(GErr::conflict(
                "send_as_unavailable",
                "this key's sending agent is not active",
            ));
        }
        let sender = state
            .store
            .get(send_as.clone())
            .await?
            .ok_or_else(|| GErr::conflict("send_as_unavailable", "this key's sending agent is not active"))?;
        // Resolve the recipient through the same door every send uses, then require it to be this
        // org's own live mailbox.
        let recipient = match crate::resolve_recipient(&state, &to).await? {
            Some(r) => r,
            None => {
                return Err(GErr::new(
                    StatusCode::NOT_FOUND,
                    "recipient_unresolved",
                    format!("no mailbox at {to}"),
                ))
            }
        };
        let in_org = {
            let (o2, a) = (o.id.clone(), recipient.address.clone());
            state
                .store
                .group_read(move |c: &Connection| -> GResult<bool> {
                    Ok(c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM org_members WHERE org_id = ?1 AND identity = ?2 AND status = 'active')
                             OR EXISTS(SELECT 1 FROM org_agents WHERE org_id = ?1 AND identity = ?2 AND status = 'active')",
                        params![o2, a],
                        |r| r.get(0),
                    )?)
                })
                .await?
        };
        if !in_org {
            return Err(GErr::forbidden(
                "recipient_not_in_org",
                "an org key sends only to this organization's own mailboxes",
            ));
        }
        rate_limit(&format!("orgmsg:{}", ctx.key_id), 1_000, 3_600)?;
        let extras = crate::msgactions::SendExtras {
            msg_ref: idem.0.as_ref().map(|i| msg_ref_for(&ctx.key_id, &i.key)),
            ..Default::default()
        };
        let thread = match req.thread_title.as_deref() {
            Some(t) => crate::ThreadRequest::New(t),
            None => crate::ThreadRequest::Default,
        };
        let sent = crate::do_send_with(&state, &sender, &recipient.address, &req.body, thread, &extras, false).await?;
        let now = now_unix();
        let out = ApiOut::created(json!({
            "message_id": sent["message_id"],
            "msg_ref": sent["msg_ref"],
            "from": sender.handle.clone().unwrap_or_else(|| sender.address.clone()),
            "to": recipient.handle.clone().unwrap_or_else(|| recipient.address.clone()),
            "sent_at": iso(Some(now as i64)),
        }));
        let (vault, auditor, ip, c2) = (state.vault.clone(), state.auditor.clone(), ip_of(&cip), ctx.clone());
        let to_addr = recipient.address.clone();
        let msg_ref = sent["msg_ref"].as_str().unwrap_or_default().to_string();
        let idem2 = idem.0.clone();
        let body = out.body.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                audit_by_key(tx, &auditor, &c2, "message.send", Some(&to_addr),
                    json!({ "msg_ref": msg_ref, "from": c2.send_as }), ip.as_deref(), now)?;
                if let Some(i) = &idem2 {
                    record_in(tx, &vault, i, &ApiOut { status: 201, body, ..ApiOut::default() }, now)?;
                }
                Ok(())
            })
            .await?;
        Ok(out)
    }
    .await;
    respond(&state, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_iso_8601_utc() {
        assert_eq!(iso(Some(1_793_095_200)), json!("2026-10-27T10:00:00Z"));
        assert_eq!(iso(None), Value::Null);
        assert_eq!(parse_iso("2026-10-27T10:00:00Z"), Some(1_793_095_200));
        assert_eq!(parse_iso("2026-10-27T13:00:00+03:00"), Some(1_793_095_200));
        assert_eq!(parse_iso("yesterday"), None);
    }

    #[test]
    fn cursors_round_trip_and_reject_forgeries() {
        let p = Page {
            cursor: Some(encode_cursor("mem_abc")),
            ..Page::default()
        };
        assert_eq!(decode_cursor(&p).unwrap().as_deref(), Some("mem_abc"));
        for bad in ["zz", "abc", &crate::hex_str(b"v2|x")] {
            let p = Page {
                cursor: Some(bad.to_string()),
                ..Page::default()
            };
            assert_eq!(decode_cursor(&p).unwrap_err().code, "invalid_cursor");
        }
    }

    #[test]
    fn rate_windows_count_reads_and_writes_separately() {
        let key = format!("okey_rate_{}", crate::groups::rand_b32(8));
        let t = 1_000_000;
        for i in 0..RATE_WRITES {
            let r = hit(&key, true, t).unwrap();
            assert_eq!(r.limit, RATE_WRITES);
            assert_eq!(r.remaining, RATE_WRITES - i - 1);
        }
        let denied = hit(&key, true, t + 5).unwrap_err();
        assert_eq!(denied.remaining, 0);
        assert_eq!(denied.reset, 55);
        // Reads still pass until the overall ceiling.
        let r = hit(&key, false, t + 5).unwrap();
        assert_eq!(r.limit, RATE_ALL);
        assert_eq!(r.remaining, RATE_ALL - RATE_WRITES - 1);
        // A new window starts fresh.
        assert!(hit(&key, true, t + RATE_WINDOW).is_ok());
    }

    #[test]
    fn idempotent_sends_derive_a_valid_msg_ref() {
        let a = msg_ref_for("okey_1", "build-42");
        assert!(crate::msgactions::is_msg_ref(&a), "{a}");
        assert_eq!(a, msg_ref_for("okey_1", "build-42"));
        assert_ne!(a, msg_ref_for("okey_2", "build-42"));
    }
}
