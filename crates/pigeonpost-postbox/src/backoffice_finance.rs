//! The back office's credit screens: a proxy from staff to saas_be marketplace credits/finance
//! (plan `docs/planning/2026-11-11-backoffice-plan.md`, Money › Credits).
//!
//! The browser never holds a finance credential. The postbox calls saas_be itself with a tenant
//! service token kept in a root-only file (`POSTBOX_SAAS_FINANCE_TOKEN_FILE`, like the bridge and
//! attestation secrets), at `POSTBOX_SAAS_FINANCE_URL` — the tenant's marketplace base,
//! `https://…/v1/internal/tenants/{tenant}/marketplace`. Not through the billing bridge: the bridge
//! is the public member API and deliberately holds only `marketplace:bridge`, never
//! `marketplace:finance`; putting the finance token there would hand refunds and adjustments to the
//! one internet-facing service that does not need them.
//!
//! saas_be's token entry for this credential needs scopes `marketplace:finance` (sales, refunds,
//! resolve, adjustments, payout profiles) and `marketplace:bridge` (per-owner balance and history
//! are bridge-scoped there). This module calls only the fixed upstream routes below, whatever the
//! token could do; holds, captures, releases and checkouts are never reachable from here.
//!
//! | Route | Upstream | Action | Audit |
//! |---|---|---|---|
//! | `GET /v1/staff/credits/owners/{kind}/{id}/balance` | `credits/balances/{kind}/{id}` | `StaffFinanceRead` | `staff.credits.balance` |
//! | `GET /v1/staff/credits/owners/{kind}/{id}/history` | `credits/history/{kind}/{id}` | `StaffFinanceRead` | `staff.credits.history` |
//! | `GET /v1/staff/credits/sales` | `finance/credit-sales` | `StaffFinanceRead` | `staff.credits.sales` |
//! | `GET /v1/staff/credits/refunds` | `finance/credit-refunds` | `StaffFinanceRead` | `staff.credits.refunds` |
//! | `GET /v1/staff/publishers/{publisher}/payout-profile` | `finance/publishers/{id}/payout-profile` | `StaffFinanceRead` | `staff.publishers.payout_profile` |
//! | `POST /v1/staff/credits/purchases/{purchase}/refund` | `credits/purchases/{id}/refund` | `StaffFinanceWrite` | `staff.credits.refund` |
//! | `POST /v1/staff/credits/refunds/{refund}/resolve` | `credits/refunds/{id}/resolve` | `StaffFinanceWrite` | `staff.credits.resolve` |
//! | `POST /v1/staff/credits/adjustments` | `credits/adjustments` | `StaffFinanceWrite` | `staff.credits.adjust` |
//!
//! Reads are recorded before saas_be is asked, so a read that leaves the postbox always has its
//! row. A write is recorded twice: `phase: requested` before the call (no row, no call) and its
//! outcome after. saas_be records the same actor (`staff:<account>`) in its own journal.

use crate::backoffice::{audit_row, page_of, require_staff, MAX_LIMIT};
use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, parse};
use crate::policy::Action;
use crate::{AppState, ClientIp};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// The largest staff adjustment, either way: $10,000. saas_be allows more; a back office
/// correction larger than this is not a support action and goes through finance by hand.
pub(crate) const MAX_ADJUST_CENTS: i64 = 1_000_000;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/staff/credits/owners/{kind}/{owner}/balance",
            get(balance),
        )
        .route(
            "/v1/staff/credits/owners/{kind}/{owner}/history",
            get(history),
        )
        .route("/v1/staff/credits/sales", get(sales))
        .route("/v1/staff/credits/refunds", get(refunds))
        .route(
            "/v1/staff/publishers/{publisher}/payout-profile",
            get(payout_profile),
        )
        .route(
            "/v1/staff/credits/purchases/{purchase}/refund",
            post(refund),
        )
        .route("/v1/staff/credits/refunds/{refund}/resolve", post(resolve))
        .route("/v1/staff/credits/adjustments", post(adjust))
}

// ---- the upstream client -------------------------------------------------------------------------

enum Transport {
    Disabled,
    Http {
        url: String,
        token: String,
        client: reqwest::Client,
    },
}

/// saas_be marketplace finance, for staff. `Disabled` answers every route `503
/// finance_unavailable`.
pub(crate) struct Finance {
    transport: Transport,
}

impl Default for Finance {
    fn default() -> Self {
        Finance {
            transport: Transport::Disabled,
        }
    }
}

impl Finance {
    /// `POSTBOX_SAAS_FINANCE_URL` and the token in `POSTBOX_SAAS_FINANCE_TOKEN_FILE`. There is no
    /// plain-variable fallback for the token: it moves money.
    pub(crate) fn from_env() -> Self {
        let url = std::env::var("POSTBOX_SAAS_FINANCE_URL").unwrap_or_default();
        let token = match std::env::var("POSTBOX_SAAS_FINANCE_TOKEN_FILE") {
            Ok(path) if !path.trim().is_empty() => {
                std::fs::read_to_string(path.trim()).unwrap_or_default()
            }
            _ => String::new(),
        };
        if url.trim().is_empty() && token.trim().is_empty() {
            return Finance::default();
        }
        let f = Finance::http(&url, &token);
        if !f.configured() {
            tracing::warn!(
                "POSTBOX_SAAS_FINANCE_URL/POSTBOX_SAAS_FINANCE_TOKEN_FILE unusable; back office credits off"
            );
        }
        f
    }

    /// `https://…` (or plain HTTP to loopback / a private address) and a token of 32+ characters.
    pub(crate) fn http(url: &str, token: &str) -> Self {
        let url = url.trim().trim_end_matches('/').to_string();
        let token = token.trim().to_string();
        if !crate::nests::url_ok(&url) || token.len() < 32 {
            return Finance::default();
        }
        match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => Finance {
                transport: Transport::Http { url, token, client },
            },
            Err(_) => Finance::default(),
        }
    }

    pub(crate) fn configured(&self) -> bool {
        matches!(self.transport, Transport::Http { .. })
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> GResult<(u16, Value)> {
        let Transport::Http { url, token, client } = &self.transport else {
            return Err(unavailable());
        };
        let mut req = client
            .request(method, format!("{url}{path}"))
            .bearer_auth(token)
            .query(query);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|_| unavailable())?;
        let status = resp.status().as_u16();
        let v = resp.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, v))
    }
}

fn unavailable() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "finance_unavailable",
        "the finance service is not configured or not answering",
    )
}

/// saas_be's refusal, as one of a few stable codes. Its message is passed on (it says what to
/// fix), shortened; nothing else of the body is.
fn refusal(status: u16, v: &Value) -> GErr {
    let message = match &v["message"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("; "),
        _ => v["error"]["message"]
            .as_str()
            .unwrap_or("the finance service refused this")
            .to_string(),
    };
    let message: String = message.chars().take(300).collect();
    match status {
        400 => GErr::bad("finance_refused", message),
        404 => GErr::new(StatusCode::NOT_FOUND, "not_found", message),
        409 => GErr::conflict("finance_conflict", message),
        422 => GErr::new(StatusCode::UNPROCESSABLE_ENTITY, "finance_refused", message),
        _ => unavailable(),
    }
}

// ---- validation ---------------------------------------------------------------------------------

/// saas_be's `ID` pattern: `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$`.
fn id_ok(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

fn key_ok(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 120
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

fn uuid_ok(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn owner(kind: &str, id: &str) -> GResult<()> {
    if !matches!(kind, "account" | "org") || !id_ok(id) {
        return Err(GErr::bad(
            "invalid_request",
            "owner: kind account or org, and an id",
        ));
    }
    Ok(())
}

fn reason(r: &str) -> GResult<String> {
    let r = r.trim().to_string();
    if r.is_empty() || r.chars().count() > 255 {
        return Err(GErr::bad("invalid_request", "reason: 1-255 characters"));
    }
    Ok(r)
}

/// Who saas_be records as having done it.
fn actor(account: &str) -> String {
    format!("staff:{account}")
}

fn rfc3339(q: &HashMap<String, String>, key: &'static str) -> GResult<Option<String>> {
    match q.get(key).map(|s| s.trim()) {
        None | Some("") => Ok(None),
        Some(v) if chrono::DateTime::parse_from_rfc3339(v).is_ok() => Ok(Some(v.to_string())),
        Some(_) => Err(GErr::bad(
            "invalid_request",
            format!("{key}: an RFC 3339 time"),
        )),
    }
}

/// `items` of an upstream list, paged here the same way as every back office list.
fn page_items(v: &Value, q: &HashMap<String, String>) -> GResult<Value> {
    let page = page_of(q, MAX_LIMIT)?;
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let start = (page.offset as usize).min(items.len());
    let end = (start + page.limit as usize).min(items.len());
    let next = (end < items.len()).then(|| format!("o{end}"));
    Ok(json!({ "data": items[start..end].to_vec(), "next_cursor": next }))
}

// ---- reads -------------------------------------------------------------------------------------

/// Record the read, then ask saas_be.
#[allow(clippy::too_many_arguments)]
async fn proxy_read(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: &Option<Extension<ClientIp>>,
    verb: &str,
    target: Option<String>,
    details: Value,
    path: String,
    query: Vec<(&str, String)>,
) -> GResult<Value> {
    let who = require_staff(state, headers, Action::StaffFinanceRead, "credits").await?;
    if !state.staff_finance.configured() {
        return Err(unavailable());
    }
    let row = audit_row(state, &who, verb, target, details, client_ip, headers);
    state.store.append_audit(row).await?;
    let (status, v) = state
        .staff_finance
        .call(reqwest::Method::GET, &path, &query, None)
        .await?;
    if !(200..300).contains(&status) {
        return Err(refusal(status, &v));
    }
    Ok(v)
}

/// `GET /v1/staff/credits/owners/{kind}/{owner}/balance` — available and held cents.
async fn balance(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
) -> Response {
    let result = async {
        owner(&kind, &id)?;
        let v = proxy_read(
            &state,
            &headers,
            &client_ip,
            "staff.credits.balance",
            Some(format!("{kind}:{id}")),
            json!({}),
            format!("/credits/balances/{kind}/{id}"),
            vec![],
        )
        .await?;
        Ok(Json(json!({ "data": {
            "owner_kind": kind,
            "owner_id": id,
            "currency": v["currency"],
            "available_cents": v["available_cents"],
            "held_cents": v["held_cents"],
        }})))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/credits/owners/{kind}/{owner}/history?cursor=&limit=` — purchases (channel
/// currency, amount, FX, USD credited, status), holds, sales, refunds and adjustments, newest first.
/// saas_be's own cursor is passed through.
async fn history(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        owner(&kind, &id)?;
        let limit = page_of(
            &q.get("limit")
                .map(|l| HashMap::from([("limit".to_string(), l.clone())]))
                .unwrap_or_default(),
            MAX_LIMIT,
        )?
        .limit;
        let mut query = vec![("limit", limit.to_string())];
        if let Some(c) = q.get("cursor").filter(|c| !c.is_empty()) {
            if c.len() > 256
                || !c
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "._:=-".contains(ch))
            {
                return Err(GErr::bad(
                    "invalid_cursor",
                    "this cursor is not one we issued",
                ));
            }
            query.push(("cursor", c.clone()));
        }
        let v = proxy_read(
            &state,
            &headers,
            &client_ip,
            "staff.credits.history",
            Some(format!("{kind}:{id}")),
            json!({}),
            format!("/credits/history/{kind}/{id}"),
            query,
        )
        .await?;
        Ok(Json(json!({
            "data": v["items"].as_array().cloned().unwrap_or_default(),
            "next_cursor": v["next_cursor"].as_str(),
        })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/credits/sales?publisher_id=&owner_kind=&owner_id=&from=&to=` — credit sales with
/// their commission split, newest first (saas_be returns at most 500).
async fn sales(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let mut query = Vec::new();
        for key in ["publisher_id", "owner_id"] {
            if let Some(v) = q.get(key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                if !id_ok(v) {
                    return Err(GErr::bad("invalid_request", format!("{key}: an id")));
                }
                query.push((key, v.to_string()));
            }
        }
        if let Some(k) = q
            .get("owner_kind")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            if !matches!(k, "account" | "org") {
                return Err(GErr::bad("invalid_request", "owner_kind: account or org"));
            }
            query.push(("owner_kind", k.to_string()));
        }
        for key in ["from", "to"] {
            if let Some(v) = rfc3339(&q, key)? {
                query.push((key, v));
            }
        }
        let details = Value::Object(
            query
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect(),
        );
        let v = proxy_read(
            &state,
            &headers,
            &client_ip,
            "staff.credits.sales",
            None,
            details,
            "/finance/credit-sales".into(),
            query,
        )
        .await?;
        Ok(Json(page_items(&v, &q)?))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/credits/refunds?status=pending|succeeded|failed|uncertain` — credit refunds,
/// newest first. `uncertain` ones are the stuck refunds that need resolving.
async fn refunds(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let status = q
            .get("status")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if status
            .as_deref()
            .is_some_and(|s| !matches!(s, "pending" | "succeeded" | "failed" | "uncertain"))
        {
            return Err(GErr::bad(
                "invalid_request",
                "status: pending, succeeded, failed or uncertain",
            ));
        }
        let query = status
            .clone()
            .map(|s| vec![("status", s)])
            .unwrap_or_default();
        let v = proxy_read(
            &state,
            &headers,
            &client_ip,
            "staff.credits.refunds",
            None,
            json!({ "status": status }),
            "/finance/credit-refunds".into(),
            query,
        )
        .await?;
        Ok(Json(page_items(&v, &q)?))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/publishers/{publisher}/payout-profile` — a publisher's payout details as saas_be
/// keeps them: the IBAN's last four digits only (saas_be never returns more), bank, country,
/// currency and verification state.
async fn payout_profile(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
) -> Response {
    let result = async {
        if !id_ok(&publisher) {
            return Err(GErr::bad("invalid_request", "publisher: an id"));
        }
        let v = proxy_read(
            &state,
            &headers,
            &client_ip,
            "staff.publishers.payout_profile",
            Some(format!("publisher:{publisher}")),
            json!({}),
            format!("/finance/publishers/{publisher}/payout-profile"),
            vec![],
        )
        .await?;
        let last4 = v["iban_last4"].as_str().unwrap_or_default();
        Ok(Json(json!({ "data": {
            "publisher_id": publisher,
            "iban": if last4.is_empty() { Value::Null } else { json!(format!("TR•• •••• •••• •••• •••• ••{last4}")) },
            "bank_name": v["bank_name"],
            "country": v["country"],
            "currency": v["currency"],
            "status": v["status"],
            "verified_at": v["verified_at"],
            "verified_by": v["verified_by"],
        }})))
    }
    .await;
    into_response(result)
}

// ---- writes ------------------------------------------------------------------------------------

/// Record the request, call saas_be, record the outcome; answer with saas_be's result.
#[allow(clippy::too_many_arguments)]
async fn proxy_write(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: &Option<Extension<ClientIp>>,
    verb: &str,
    target: String,
    details: Value,
    path: String,
    body: impl FnOnce(&str) -> Value,
) -> GResult<Value> {
    let who = require_staff(state, headers, Action::StaffFinanceWrite, "credits").await?;
    if !state.staff_finance.configured() {
        return Err(unavailable());
    }
    let mut requested = details.clone();
    requested["phase"] = json!("requested");
    let row = audit_row(
        state,
        &who,
        verb,
        Some(target.clone()),
        requested,
        client_ip,
        headers,
    );
    state.store.append_audit(row).await?;
    let outcome = state
        .staff_finance
        .call(
            reqwest::Method::POST,
            &path,
            &[],
            Some(body(&actor(&who.account))),
        )
        .await;
    let mut done = details;
    let answer = match outcome {
        Ok((status, v)) if (200..300).contains(&status) => {
            done["phase"] = json!("completed");
            Ok(v)
        }
        Ok((status, v)) => {
            done["phase"] = json!("refused");
            done["status"] = json!(status);
            Err(refusal(status, &v))
        }
        Err(e) => {
            done["phase"] = json!("unavailable");
            Err(e)
        }
    };
    let row = audit_row(state, &who, verb, Some(target), done, client_ip, headers);
    if let Err(e) = state.store.append_audit(row).await {
        // The money moved (or did not) either way; the `requested` row is already there.
        tracing::error!(error = %e, verb, "back office: could not record a finance outcome");
    }
    answer
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefundReq {
    reason: String,
}

/// `POST /v1/staff/credits/purchases/{purchase}/refund` `{reason}` — refund what is left of a
/// credit purchase to the card it was paid with (saas_be decides how much is refundable).
async fn refund(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(purchase): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        if !uuid_ok(&purchase) {
            return Err(GErr::bad("invalid_request", "purchase: a purchase id"));
        }
        let req: RefundReq = parse(body)?;
        let why = reason(&req.reason)?;
        let v = proxy_write(
            &state,
            &headers,
            &client_ip,
            "staff.credits.refund",
            format!("credit_purchase:{purchase}"),
            json!({ "reason": why }),
            format!("/credits/purchases/{purchase}/refund"),
            |actor| json!({ "reason": why, "requested_by": actor }),
        )
        .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveReq {
    outcome: String,
    reason: String,
    #[serde(default)]
    provider_reference: Option<String>,
}

/// `POST /v1/staff/credits/refunds/{refund}/resolve` `{outcome: succeeded|failed, reason,
/// provider_reference?}` — close a stuck refund after checking the iyzico panel: `succeeded`
/// closes it, `failed` gives the buyer the credit back.
async fn resolve(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(refund): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        if !uuid_ok(&refund) {
            return Err(GErr::bad("invalid_request", "refund: a refund id"));
        }
        let req: ResolveReq = parse(body)?;
        if !matches!(req.outcome.as_str(), "succeeded" | "failed") {
            return Err(GErr::bad("invalid_request", "outcome: succeeded or failed"));
        }
        let why = reason(&req.reason)?;
        let reference = req
            .provider_reference
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        if reference
            .as_deref()
            .is_some_and(|r| r.chars().count() > 128)
        {
            return Err(GErr::bad(
                "invalid_request",
                "provider_reference: at most 128 characters",
            ));
        }
        let outcome = req.outcome.clone();
        let v = proxy_write(
            &state,
            &headers,
            &client_ip,
            "staff.credits.resolve",
            format!("credit_refund:{refund}"),
            json!({ "outcome": outcome, "reason": why }),
            format!("/credits/refunds/{refund}/resolve"),
            |actor| {
                let mut b = json!({ "outcome": req.outcome, "reason": why, "actor": actor });
                if let Some(r) = reference {
                    b["provider_reference"] = json!(r);
                }
                b
            },
        )
        .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdjustReq {
    owner_kind: String,
    owner_id: String,
    amount_cents: i64,
    reason: String,
    /// Made by the form when it opens, so a resubmitted form adjusts once.
    idempotency_key: String,
}

/// `POST /v1/staff/credits/adjustments` — add (positive) or remove (negative) credit, in USD
/// cents, at most [`MAX_ADJUST_CENTS`] either way, with a reason. Idempotent on the key.
async fn adjust(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: AdjustReq = parse(body)?;
        owner(&req.owner_kind, &req.owner_id)?;
        if req.amount_cents == 0 || req.amount_cents.abs() > MAX_ADJUST_CENTS {
            return Err(GErr::bad(
                "invalid_request",
                format!("amount_cents: not zero, at most {MAX_ADJUST_CENTS} either way"),
            ));
        }
        if !key_ok(&req.idempotency_key) {
            return Err(GErr::bad(
                "invalid_request",
                "idempotency_key: 1-120 of A-Z a-z 0-9 . _ : -",
            ));
        }
        let why = reason(&req.reason)?;
        let key = format!("bo:{}", req.idempotency_key);
        let (kind, id, cents) = (
            req.owner_kind.clone(),
            req.owner_id.clone(),
            req.amount_cents,
        );
        let v = proxy_write(
            &state,
            &headers,
            &client_ip,
            "staff.credits.adjust",
            format!("{kind}:{id}"),
            json!({ "amount_cents": cents, "reason": why, "idempotency": key }),
            "/credits/adjustments".into(),
            |actor| {
                json!({
                    "owner": { "owner_kind": kind, "owner_id": id },
                    "amount_cents": cents,
                    "reason": why,
                    "actor": actor,
                    "idempotency_key": key,
                })
            },
        )
        .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_saas_be_patterns() {
        assert!(id_ok("acct_abc123"));
        assert!(id_ok("org_x.y:z-1"));
        for bad in ["", "_x", "a/b", "a b", &"a".repeat(65)] {
            assert!(!id_ok(bad), "{bad:?}");
        }
        assert!(uuid_ok("0f8fad5b-d9cb-469f-a165-70867728950e"));
        for bad in [
            "0f8fad5b",
            "0f8fad5b-d9cb-469f-a165-70867728950",
            "../x",
            "zf8fad5b-d9cb-469f-a165-70867728950e",
        ] {
            assert!(!uuid_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn refusals_map_to_stable_codes_and_never_leak_the_body() {
        let e = refusal(400, &json!({ "message": ["a", "b"], "secret": "x" }));
        assert_eq!(
            (e.status.as_u16(), e.code, e.message.as_str()),
            (400, "finance_refused", "a; b")
        );
        assert_eq!(refusal(409, &json!({})).code, "finance_conflict");
        assert_eq!(refusal(500, &json!({})).code, "finance_unavailable");
        assert_eq!(refusal(401, &json!({})).code, "finance_unavailable");
    }

    #[test]
    fn a_short_or_insecure_configuration_is_off() {
        assert!(!Finance::http("https://saas.example/v1", "short").configured());
        assert!(!Finance::http("http://saas.example/v1", &"t".repeat(40)).configured());
        assert!(Finance::http("https://saas.example/v1", &"t".repeat(40)).configured());
        assert!(Finance::http("http://127.0.0.1:9/v1", &"t".repeat(40)).configured());
    }
}
