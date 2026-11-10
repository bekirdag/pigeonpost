//! Marketplace M1 (build plan §8, "reference path"): a first-party release goes from its build to
//! an attested, admitted release that serves native jobs from a shared listing.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `POST /v1/nest-jobs/quote` | `NestInvoke` | any credential but a connector; zero-priced only |
//! | `POST /v1/nest-payment-authorizations` | `NestInvoke` | same; zero-priced, single use, 5 minutes |
//! | `POST /v1/nest-jobs` | `NestInvoke` | same, with the quote's authorization and an `Idempotency-Key` |
//! | `GET /v1/nest-jobs/{job}` | `NestJobRead` | the submitting account |
//! | `GET /v1/staff/reviews/{review}` | `NestReview` | a reviewer with step-up and no stake |
//! | `POST /v1/staff/reviews/{review}/decisions` | `NestReview`, `NestAttest` | same |
//! | `POST /v1/staff/releases/{release}/security` | `NestQuarantine` | same |
//! | `POST /internal/v1/marketplace/builds` | ops service | the build plane |
//! | `POST /internal/v1/marketplace/reviews/{review}/stages` | ops service | the analysis plane |
//!
//! No money moves in M1: every quote, authorization and price is zero, and the schema refuses
//! anything else. There is no wallet and no mandate (owner decision 2026-11-09).
//!
//! Every member route answers `404 feature_disabled` unless the `marketplace` flag is on for the
//! caller's account; the internal routes need it on for the platform owner's account.

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, feature_off, into_response, ip_of, iso, not_found};
use crate::policy::{authorize, Action, Actor, Facts, Resource, StaffRole};
use crate::store::marketplace as store;
use crate::{capabilities, now_unix, AppState, ClientIp, Principal};
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// The marketplace currency (OD-5). A currency, not a price: every amount here is zero.
pub(crate) const CURRENCY: &str = "TRY";
/// Quotes and payment authorizations live five minutes.
pub(crate) const QUOTE_SECS: u64 = 300;
pub(crate) const AUTHORIZATION_SECS: u64 = 300;
/// Request limits of `POST /v1/nest-jobs` (contract).
pub(crate) const MAX_JOB_BYTES: usize = 256 * 1024;
pub(crate) const MAX_JSON_DEPTH: usize = 32;
/// Furthest a job deadline may be in M1.
const MAX_DEADLINE_SECS: u64 = 7 * 86_400;
/// The one model the AI review may name (self-hosted, through mswarm).
pub(crate) const AI_REVIEW_MODEL: &str = "qwen-latest-flash-next";
/// Who automated stages are recorded as. Neither is an account, so neither can hold a staff role.
const BUILD_PLANE: &str = "system:build-plane";
const ANALYSIS_PLANE: &str = "system:analysis";
/// The platform publisher's owner id: Pigeonpost itself, not an account.
const PLATFORM_OWNER_ID: &str = "pigeonpost";
/// The release digest's domain separator; `pigeonpost_nests::release` computes the same.
const RELEASE_DIGEST_CONTEXT: &str = "pigeonpost-nest-release-v1";
const STATEMENT_TYPE: &str = "pigeonpost.nest.release.attestation.v1";

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nest-jobs/quote", post(quote))
        .route("/v1/nest-payment-authorizations", post(authorize_payment))
        .route("/v1/nest-jobs", post(submit_job))
        .route("/v1/nest-jobs/{job}", get(read_job))
        .route("/v1/staff/reviews/{review}", get(staff_read_review))
        .route("/v1/staff/reviews/{review}/decisions", post(staff_decide))
        .route(
            "/v1/staff/releases/{release}/security",
            post(staff_security),
        )
}

pub(crate) fn internal_router() -> Router<AppState> {
    Router::new()
        .route("/internal/v1/marketplace/builds", post(register_build))
        .route(
            "/internal/v1/marketplace/reviews/{review}/stages",
            post(record_stage),
        )
}

// ---- small helpers --------------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn digest_of(data: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(data))
}

fn sha256_ok(d: &str) -> bool {
    d.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// JSON with object keys sorted at every level, no whitespace: the form everything hashed here
/// takes (`pigeonpost_nests::release::canonical_json` is the same).
pub(crate) fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        Value::String((*k).clone()),
                        canonical_json(&map[k.as_str()])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// The one digest that identifies a release (`pigeonpost_nests::release::release_digest`).
pub(crate) fn release_digest(
    source: &str,
    vendor: &str,
    manifest: &str,
    sbom: &str,
    image: &str,
) -> String {
    digest_of(
        format!(
            "{RELEASE_DIGEST_CONTEXT}\nsource={source}\nvendor={vendor}\nmanifest={manifest}\nsbom={sbom}\nimage={image}\n"
        )
        .as_bytes(),
    )
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// A fresh payment authorization reference: 256 random bits.
fn new_reference() -> String {
    let mut bytes = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    let r = format!("payauth_{}", base64url(&bytes));
    zeroize::Zeroize::zeroize(&mut bytes);
    r
}

fn pattern_ok(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

fn operation_ok(op: &str) -> bool {
    !op.is_empty()
        && op.len() <= 80
        && op.split('.').all(|w| {
            w.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

fn version_ok(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (2..=3).contains(&parts.len())
        && (3..=32).contains(&v.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn trace_ok(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-".contains(&b))
}

/// The buyer context a job runs in: one per listing and paying account, so two buyers of one
/// listing never share a worker.
pub(crate) fn buyer_context(listing: &str, account: &str) -> String {
    let h = sha256_hex(format!("pigeonpost-buyer-context\0{listing}\0{account}").as_bytes());
    format!("ctx-{}", &h[..24])
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    header(headers, "user-agent")
}

fn conflict_code(code: &'static str, message: &str) -> GErr {
    GErr::conflict(code, message.to_string())
}

/// A credential that could ever invoke: a person, an account key, or a mailbox token. Checked
/// before anything is looked up, so a connector learns nothing about listings.
fn precheck_invoke(principal: &Principal, state: &AppState) -> GResult<()> {
    let f = Facts {
        job_admitted: true,
        ..facts(state)
    };
    authorize(
        &Actor::Principal(principal),
        Action::NestInvoke,
        &Resource::NestOperation(""),
        &f,
    )
    .map_err(|d| deny(d, false))
}

/// The acting mailbox: a capability token's own, or the one `x-pigeonpost-identity` names for an
/// account credential. `None` when an account acts without naming one.
async fn acting_of(
    state: &AppState,
    principal: &Principal,
    headers: &HeaderMap,
) -> GResult<Option<String>> {
    let explicit = header(headers, "x-pigeonpost-identity");
    match principal {
        Principal::Identity(id) => {
            if let Some(e) = explicit {
                if e != id.address && Some(e) != id.handle.as_deref() {
                    return Err(GErr::forbidden(
                        "forbidden",
                        "a capability token acts as its own mailbox",
                    ));
                }
            }
            Ok(Some(id.address.clone()))
        }
        Principal::Account(account, credential) => match explicit {
            None => Ok(None),
            Some(e) => {
                let id = crate::resolve_acting_identity(
                    state,
                    Principal::Account(account.clone(), credential.clone()),
                    Some(e),
                )
                .await?;
                Ok(Some(id.address))
            }
        },
    }
}

// ---- strict JSON -----------------------------------------------------------------------------------

/// Parse JSON refusing duplicate object keys and nesting deeper than [`MAX_JSON_DEPTH`].
struct Strict {
    depth: usize,
}

const TOO_DEEP: &str = "nested deeper than the limit";

impl<'de> serde::de::DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Strict {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom(TOO_DEEP));
        }
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Strict {
            depth: self.depth + 1,
        })? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom(TOO_DEEP));
        }
        let mut out = Map::new();
        while let Some(k) = map.next_key::<String>()? {
            if out.contains_key(&k) {
                return Err(serde::de::Error::custom("duplicate object key"));
            }
            let v = map.next_value_seed(Strict {
                depth: self.depth + 1,
            })?;
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }
}

pub(crate) fn strict_json(bytes: &[u8]) -> GResult<Value> {
    if bytes.len() > MAX_JOB_BYTES {
        return Err(GErr::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "the request body is over 256 KiB",
        ));
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = serde::de::DeserializeSeed::deserialize(Strict { depth: 0 }, &mut de)
        .and_then(|v| de.end().map(|_| v));
    match value {
        Ok(v) => Ok(v),
        Err(e) if e.to_string().contains(TOO_DEEP) => Err(GErr::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "the request is nested deeper than 32 levels",
        )),
        Err(e) => Err(GErr::bad("invalid_json", e.to_string())),
    }
}

// ---- idempotency ------------------------------------------------------------------------------------

fn idempotency_key(headers: &HeaderMap) -> GResult<String> {
    let key = header(headers, "idempotency-key").ok_or_else(|| {
        GErr::bad(
            "invalid_request",
            "Idempotency-Key is required on this route",
        )
    })?;
    if !(16..=128).contains(&key.len())
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    {
        return Err(GErr::bad(
            "invalid_request",
            "Idempotency-Key: 16-128 characters from A-Z a-z 0-9 . _ : -",
        ));
    }
    Ok(key.to_string())
}

/// A recorded response for this key, or `idempotency_conflict` when the key was used for another
/// request.
fn idempotent_in(
    c: &Connection,
    scope: &str,
    key: &str,
    request: &str,
) -> GResult<Option<(u16, Value)>> {
    let row: Option<(String, i64, String)> = c
        .query_row(
            "SELECT request_sha256, status, response FROM operation_idempotency
              WHERE scope = ?1 AND key = ?2",
            params![scope, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((hash, _, _)) if hash != request => Err(GErr::conflict(
            "idempotency_conflict",
            "this Idempotency-Key was used for a different request",
        )),
        Some((_, status, body)) => Ok(Some((
            status as u16,
            serde_json::from_str(&body).unwrap_or(Value::Null),
        ))),
    }
}

fn remember_in(
    tx: &Transaction<'_>,
    scope: &str,
    key: &str,
    request: &str,
    status: u16,
    body: &Value,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO operation_idempotency (scope, key, request_sha256, status, response, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![scope, key, request, status as i64, body.to_string(), now],
    )
    .map(|_| ())
}

fn reply(status: u16, body: Value, replayed: bool, no_store: bool) -> Response {
    let mut r = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(body),
    )
        .into_response();
    if replayed {
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    if no_store {
        r.headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
    }
    r
}

// ---- offers -----------------------------------------------------------------------------------------

/// A listing's operation and everything admission decides on.
#[derive(Debug, Clone)]
struct Offer {
    listing_id: String,
    address: String,
    listing_status: String,
    release_id: String,
    release_digest: Option<String>,
    image: Option<String>,
    security_state: String,
    input_schema: Option<String>,
    attestation: Option<(String, String)>,
    /// Hardening: `(key_id, signature)` of that attestation, and whether a kill is in force.
    signature: Option<(String, String)>,
    killed: bool,
}

fn load_offer_in(
    c: &Connection,
    address: &str,
    operation: &str,
    version: &str,
) -> rusqlite::Result<Option<Offer>> {
    let found = c
        .query_row(
            "SELECT l.id, l.address, l.status, r.id, r.release_digest, r.image, r.security_state,
                    o.input_schema
               FROM nest_listings l JOIN nest_releases r ON r.id = l.release_id
               LEFT JOIN nest_operations o ON o.listing_id = l.id AND o.operation = ?2
                    AND o.operation_version = ?3
              WHERE l.address = ?1",
            params![address, operation, version],
            |r| {
                Ok(Offer {
                    listing_id: r.get(0)?,
                    address: r.get(1)?,
                    listing_status: r.get(2)?,
                    release_id: r.get(3)?,
                    release_digest: r.get(4)?,
                    image: r.get(5)?,
                    security_state: r.get(6)?,
                    input_schema: r.get(7)?,
                    attestation: None,
                    signature: None,
                    killed: false,
                })
            },
        )
        .optional()?;
    let Some(mut offer) = found else {
        return Ok(None);
    };
    offer.attestation = attestation_in(c, &offer.release_id)?;
    offer.signature = crate::marketplace_hardening::signature_in(c, &offer.release_id)?;
    offer.killed = crate::marketplace_hardening::killed_in(c, &offer.listing_id)?;
    Ok(Some(offer))
}

/// The release's attestation statement, if it has one whose digest still matches.
fn attestation_in(c: &Connection, release: &str) -> rusqlite::Result<Option<(String, String)>> {
    let row: Option<(Option<String>, String)> = c
        .query_row(
            "SELECT statement, statement_digest FROM release_attestations
              WHERE release_id = ?1 AND statement IS NOT NULL ORDER BY created_at DESC, id LIMIT 1",
            [release],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(s, d)| s.map(|s| (s, d))))
}

/// Admission's view of a release: attested over its digest, live, and offering the operation.
fn admit<'a>(
    offer: &'a Option<Offer>,
    keys: &crate::marketplace_hardening::Hardening,
) -> GResult<&'a Offer> {
    let Some(o) = offer else {
        return Err(not_found());
    };
    // Unpublished listings are private: no answer distinguishes them from missing ones.
    if o.listing_status != "published" {
        return Err(not_found());
    }
    if o.killed {
        return Err(conflict_code(
            "listing_suspended",
            "this listing is stopped by the platform",
        ));
    }
    match o.security_state.as_str() {
        "revoked" => return Err(conflict_code("release_revoked", "this release is revoked")),
        "quarantined" => {
            return Err(conflict_code(
                "security_quarantined",
                "this release is quarantined",
            ))
        }
        _ => {}
    }
    let attested = match (&o.attestation, &o.release_digest, &o.image) {
        (Some((statement, digest)), Some(release_digest), Some(image)) => {
            digest_of(statement.as_bytes()) == *digest
                && serde_json::from_str::<Value>(statement).is_ok_and(|s| {
                    s["type"] == STATEMENT_TYPE
                        && s["release_id"] == o.release_id.as_str()
                        && s["release_digest"] == release_digest.as_str()
                        && s["image"] == image.as_str()
                        && s["decision"] == "approved"
                })
                && o.signature
                    .as_ref()
                    .is_some_and(|(kid, sig)| keys.verify(statement, kid, sig))
        }
        _ => false,
    };
    if !attested {
        return Err(conflict_code(
            "review_required",
            "this release has no valid staff attestation",
        ));
    }
    if o.input_schema.is_none() {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such operation",
        ));
    }
    Ok(o)
}

/// Check a payload against the operation's input fields: declared string fields only, within
/// their bounds, the required ones present.
fn validate_payload(schema: &str, payload: &Map<String, Value>) -> GResult<()> {
    let bad = |m: &str| GErr::bad("invalid_operation_payload", m.to_string());
    let schema: Value = serde_json::from_str(schema).map_err(|_| bad("unreadable schema"))?;
    let fields = schema["fields"].as_array().cloned().unwrap_or_default();
    for key in payload.keys() {
        if !fields.iter().any(|f| f["name"] == key.as_str()) {
            return Err(bad(&format!("{key}: not a field of this operation")));
        }
    }
    for f in &fields {
        let name = f["name"].as_str().unwrap_or("");
        let max = f["max_chars"].as_u64().unwrap_or(0) as usize;
        match payload.get(name) {
            None | Some(Value::Null) if f["required"] == true => {
                return Err(bad(&format!("{name}: required")))
            }
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                if s.trim().is_empty() && f["required"] == true {
                    return Err(bad(&format!("{name}: required")));
                }
                if s.chars().count() > max {
                    return Err(bad(&format!("{name}: longer than {max} characters")));
                }
            }
            Some(_) => return Err(bad(&format!("{name}: must be a string"))),
        }
    }
    Ok(())
}

// ---- quotes ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteReq {
    to: String,
    operation: String,
    operation_version: String,
}

/// `POST /v1/nest-jobs/quote`
async fn quote(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        precheck_invoke(&principal, &state)?;
        let req: QuoteReq = crate::marketplace::parse(body)?;
        if !operation_ok(&req.operation) || !version_ok(&req.operation_version) {
            return Err(GErr::bad(
                "invalid_request",
                "operation or version malformed",
            ));
        }
        let (to, op, ver) = (
            req.to.clone(),
            req.operation.clone(),
            req.operation_version.clone(),
        );
        let offer = state
            .store
            .group_read(move |c| -> GResult<Option<Offer>> {
                Ok(load_offer_in(c, &to, &op, &ver)?)
            })
            .await?;
        let o = admit(&offer, &state.nests.marketplace)?.clone();
        let f = Facts {
            job_admitted: true,
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::NestInvoke,
            &Resource::NestOperation(&o.listing_id),
            &f,
        )
        .map_err(|d| deny(d, false))?;
        let now = now_unix() as i64;
        let id = format!("qte_{}", crate::groups::rand_b32(16));
        let expires = now + QUOTE_SECS as i64;
        let (qid, acct) = (id.clone(), account.clone());
        let (listing, release) = (o.listing_id.clone(), o.release_id.clone());
        let (op, ver) = (req.operation.clone(), req.operation_version.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "INSERT INTO nest_quotes (id, account_id, listing_id, release_id, operation,
                        operation_version, amount_minor, currency, expires_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
                    params![qid, acct, listing, release, op, ver, CURRENCY, expires, now],
                )?;
                Ok(())
            })
            .await?;
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": {
                "quote_id": id,
                "to": o.address,
                "operation": req.operation,
                "operation_version": req.operation_version,
                "amount_minor": 0,
                "currency": CURRENCY,
                "expires_at": iso(expires),
            }})),
        ))
    }
    .await;
    into_response(result)
}

// ---- payment authorizations ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizationReq {
    quote_id: String,
    #[serde(default)]
    mandate_id: Option<String>,
}

/// `POST /v1/nest-payment-authorizations` — zero-priced only (M1). The reference is shown once
/// and stored as a hash. A retry with the same `Idempotency-Key` mints a fresh reference for the
/// same unused authorization, so a lost response never leaves a usable copy behind.
async fn authorize_payment(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result: GResult<Response> = async {
        let (principal, account) = caller(&state, &headers).await?;
        precheck_invoke(&principal, &state)?;
        let key = idempotency_key(&headers)?;
        let Json(raw) = body.map_err(|_| GErr::bad("invalid_json", "expected a JSON object"))?;
        let request = sha256_hex(canonical_json(&raw).as_bytes());
        let req: AuthorizationReq =
            serde_json::from_value(raw).map_err(|e| GErr::bad("invalid_request", e.to_string()))?;
        if req.mandate_id.is_some() {
            return Err(GErr::bad(
                "invalid_request",
                "there are no spend mandates: marketplace payments are per purchase",
            ));
        }
        if !pattern_ok(&req.quote_id, "qte_") {
            return Err(GErr::bad("invalid_request", "quote_id is malformed"));
        }
        let qid = req.quote_id.clone();
        let quote = state
            .store
            .group_read(move |c| -> GResult<Option<(String, String, i64, i64)>> {
                Ok(c.query_row(
                    "SELECT account_id, listing_id, amount_minor, expires_at FROM nest_quotes WHERE id = ?1",
                    [&qid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?)
            })
            .await?;
        // Someone else's quote reads exactly like a missing one.
        let Some((owner, listing, amount, expires)) = quote.filter(|q| q.0 == account) else {
            return Err(not_found());
        };
        let f = Facts {
            job_admitted: amount == 0,
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::NestInvoke,
            &Resource::NestOperation(&listing),
            &f,
        )
        .map_err(|d| deny(d, false))?;
        let now = now_unix() as i64;
        let scope = format!("{owner}|POST /v1/nest-payment-authorizations");
        let reference = new_reference();
        let hash = sha256_hex(reference.as_bytes());
        let auth_expires = (now + AUTHORIZATION_SECS as i64).min(expires);
        let quote_id = req.quote_id.clone();
        let body = json!({ "data": {
            "payment_authorization": reference,
            "quote_id": quote_id,
            "expires_at": iso(auth_expires),
            "amount_minor": 0,
            "currency": CURRENCY,
            "reservation_id": null,
        }});
        // What is remembered for the key: everything but the reference.
        let mut remembered = body.clone();
        remembered["data"]["payment_authorization"] = Value::Null;
        let replayed = state
            .store
            .group_tx(move |tx| -> GResult<bool> {
                if idempotent_in(tx, &scope, &key, &request)?.is_some() {
                    // Rotate: the earlier reference stops working, the new one is returned.
                    let n = tx.execute(
                        "UPDATE nest_payment_authorizations SET ref_sha256 = ?2, expires_at = ?3
                          WHERE quote_id = ?1 AND consumed_by IS NULL",
                        params![quote_id, hash, auth_expires],
                    )?;
                    if n == 0 {
                        return Err(conflict_code(
                            "authorization_reused",
                            "this authorization was already used",
                        ));
                    }
                    return Ok(true);
                }
                if expires <= now {
                    return Err(conflict_code("quote_expired", "this quote has expired"));
                }
                let used: Option<String> = tx
                    .query_row(
                        "SELECT ref_sha256 FROM nest_payment_authorizations WHERE quote_id = ?1",
                        [&quote_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if used.is_some() {
                    return Err(conflict_code(
                        "authorization_reused",
                        "this quote already has an authorization",
                    ));
                }
                tx.execute(
                    "INSERT INTO nest_payment_authorizations
                        (ref_sha256, quote_id, account_id, amount_minor, expires_at, created_at)
                     VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                    params![hash, quote_id, owner, auth_expires, now],
                )?;
                remember_in(tx, &scope, &key, &request, 201, &remembered, now)?;
                Ok(false)
            })
            .await?;
        Ok(reply(201, body, replayed, true))
    }
    .await;
    match result {
        Ok(r) => r,
        Err(e) => crate::org_api::api_error(e),
    }
}

// ---- jobs ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobReq {
    #[serde(rename = "type")]
    kind: String,
    version: u64,
    to: String,
    operation: String,
    operation_version: String,
    quote_id: String,
    payment_authorization: String,
    payload: Map<String, Value>,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    reply_to: Option<String>,
    deadline_at: String,
    #[serde(default)]
    client_trace_id: Option<String>,
}

fn validate_job(req: &JobReq) -> GResult<()> {
    let bad = |m: &str| GErr::bad("invalid_request", m.to_string());
    if req.kind != "nest.job.request" || req.version != 1 {
        return Err(bad("type must be nest.job.request, version 1"));
    }
    if req.to.is_empty() || req.to.len() > 320 {
        return Err(bad("to: 1-320 characters"));
    }
    if !operation_ok(&req.operation) || !version_ok(&req.operation_version) {
        return Err(bad("operation or operation_version malformed"));
    }
    if !pattern_ok(&req.quote_id, "qte_") {
        return Err(bad("quote_id malformed"));
    }
    let r = &req.payment_authorization;
    if !(51..=136).contains(&r.len())
        || !r.strip_prefix("payauth_").is_some_and(|b| {
            b.len() >= 43
                && b.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        })
    {
        return Err(bad("payment_authorization malformed"));
    }
    if !req.attachments.is_empty() {
        return Err(bad("attachments are not offered in M1"));
    }
    if let Some(t) = &req.client_trace_id {
        if !trace_ok(t) {
            return Err(bad("client_trace_id malformed"));
        }
    }
    Ok(())
}

fn deadline_of(text: &str, now: u64) -> GResult<i64> {
    let at = chrono::DateTime::parse_from_rfc3339(text)
        .map_err(|_| GErr::bad("deadline_invalid", "deadline_at: an ISO 8601 date-time"))?
        .timestamp();
    if at <= now as i64 || at > (now + MAX_DEADLINE_SECS) as i64 {
        return Err(GErr::bad(
            "deadline_invalid",
            "deadline_at: in the future and within seven days",
        ));
    }
    Ok(at)
}

enum Admitted {
    Created(Value),
    Replayed(u16, Value),
}

/// `POST /v1/nest-jobs`
async fn submit_job(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let result: GResult<Response> = async {
        let (principal, account) = caller(&state, &headers).await?;
        precheck_invoke(&principal, &state)?;
        let key = idempotency_key(&headers)?;
        let raw = strict_json(&body)?;
        let request = sha256_hex(canonical_json(&raw).as_bytes());
        let acting = acting_of(&state, &principal, &headers).await?;
        let scope = format!(
            "{account}|{}|POST /v1/nest-jobs",
            acting.as_deref().unwrap_or("-")
        );
        let (s2, k2, r2) = (scope.clone(), key.clone(), request.clone());
        let earlier = state
            .store
            .group_read(move |c| idempotent_in(c, &s2, &k2, &r2))
            .await?;
        if let Some((status, body)) = earlier {
            return Ok(reply(status, body, true, false));
        }
        let req: JobReq = serde_json::from_value(raw)
            .map_err(|e| GErr::bad("invalid_request", e.to_string()))?;
        validate_job(&req)?;
        if let Some(reply_to) = &req.reply_to {
            if acting.as_deref() != Some(reply_to.as_str()) {
                return Err(GErr::bad(
                    "invalid_request",
                    "reply_to: the acting mailbox, or omitted",
                ));
            }
        }
        let now = now_unix();
        let deadline = deadline_of(&req.deadline_at, now)?;

        crate::marketplace_hardening::rate_limit_jobs(&state, &account, &req.to).await?;
        let (to, op, ver) = (req.to.clone(), req.operation.clone(), req.operation_version.clone());
        let auth_hash = sha256_hex(req.payment_authorization.as_bytes());
        let (qid, ah) = (req.quote_id.clone(), auth_hash.clone());
        type Quote = (String, String, String, String, i64);
        type Auth = (String, String, Option<String>, i64);
        let (offer, quote, auth) = state
            .store
            .group_read(move |c| -> GResult<(Option<Offer>, Option<Quote>, Option<Auth>)> {
                let offer = load_offer_in(c, &to, &op, &ver)?;
                let quote = c
                    .query_row(
                        "SELECT account_id, listing_id, operation, operation_version, expires_at
                           FROM nest_quotes WHERE id = ?1",
                        [&qid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?;
                let auth = c
                    .query_row(
                        "SELECT quote_id, account_id, consumed_by, expires_at
                           FROM nest_payment_authorizations WHERE ref_sha256 = ?1",
                        [&ah],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                Ok((offer, quote, auth))
            })
            .await?;
        let o = admit(&offer, &state.nests.marketplace)?.clone();
        validate_payload(o.input_schema.as_deref().unwrap_or("{}"), &req.payload)?;
        // The quote binds the caller, the listing and the operation; the target never moves.
        let Some(q) = quote.filter(|q| q.0 == account) else {
            return Err(not_found());
        };
        if q.1 != o.listing_id || q.2 != req.operation || q.3 != req.operation_version {
            return Err(conflict_code(
                "authorization_intent_mismatch",
                "the quote is for another operation",
            ));
        }
        if q.4 <= now as i64 {
            return Err(conflict_code("quote_expired", "this quote has expired"));
        }
        let Some(a) = auth else {
            return Err(conflict_code(
                "authorization_intent_mismatch",
                "the payment authorization does not match this request",
            ));
        };
        if a.0 != req.quote_id || a.1 != account {
            return Err(conflict_code(
                "authorization_intent_mismatch",
                "the payment authorization does not match this request",
            ));
        }
        if a.2.is_some() {
            return Err(conflict_code(
                "authorization_reused",
                "this payment authorization was already used",
            ));
        }
        if a.3 <= now as i64 {
            return Err(conflict_code(
                "authorization_expired",
                "this payment authorization has expired",
            ));
        }
        let f = Facts {
            job_admitted: true,
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::NestInvoke,
            &Resource::NestOperation(&o.listing_id),
            &f,
        )
        .map_err(|d| deny(d, false))?;

        let id = format!("job_{}", crate::groups::rand_b32(16));
        let context = buyer_context(&o.listing_id, &account);
        let payload = Value::Object(req.payload.clone());
        let intent = digest_of(
            canonical_json(&json!({
                "to": o.address,
                "listing_id": o.listing_id,
                "release_id": o.release_id,
                "release_digest": o.release_digest,
                "operation": req.operation,
                "operation_version": req.operation_version,
                "quote_id": req.quote_id,
                "payload": payload,
                "deadline_at": deadline,
            }))
            .as_bytes(),
        );
        let now = now as i64;
        let accepted = json!({ "data": {
            "job_id": id,
            "admission": "admitted",
            "accepted_at": iso(now),
        }});
        let (jid, acct, ctx_id) = (id.clone(), account.clone(), context.clone());
        let (rel, listing, address) = (o.release_id.clone(), o.listing_id.clone(), o.address.clone());
        let (op, ver, qid) = (req.operation.clone(), req.operation_version.clone(), req.quote_id.clone());
        let trace = req.client_trace_id.clone();
        let body = accepted.clone();
        let outcome = state
            .store
            .group_tx(move |tx| -> GResult<Admitted> {
                if let Some((status, b)) = idempotent_in(tx, &scope, &key, &request)? {
                    return Ok(Admitted::Replayed(status, b));
                }
                // The release may have been quarantined since it was read.
                let security: String = tx.query_row(
                    "SELECT security_state FROM nest_releases WHERE id = ?1",
                    [&rel],
                    |r| r.get(0),
                )?;
                if security != "normal" {
                    return Err(conflict_code(
                        "security_quarantined",
                        "this release is no longer admitted",
                    ));
                }
                let used = tx.execute(
                    "UPDATE nest_payment_authorizations SET consumed_by = ?2, consumed_at = ?3
                      WHERE ref_sha256 = ?1 AND consumed_by IS NULL AND expires_at > ?3",
                    params![auth_hash, jid, now],
                )?;
                if used == 0 {
                    return Err(conflict_code(
                        "authorization_reused",
                        "this payment authorization was already used",
                    ));
                }
                tx.execute(
                    "INSERT INTO nest_jobs (id, account_id, acting, buyer_context, listing_id, address,
                        release_id, operation, operation_version, quote_id, authorization_sha256,
                        intent_digest, payload, client_trace_id, admission, execution, settlement,
                        revision, deadline_at, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                        'admitted', 'queued', 'none', 1, ?15, ?16, ?16)",
                    params![
                        jid, acct, acting, ctx_id, listing, address, rel, op, ver, qid, auth_hash,
                        intent, payload.to_string(), trace, deadline, now
                    ],
                )?;
                job_event_in(tx, &jid, "nest.job.admitted", json!({ "execution": "queued" }), now)?;
                remember_in(tx, &scope, &key, &request, 202, &body, now)?;
                Ok(Admitted::Created(body))
            })
            .await?;
        match outcome {
            Admitted::Replayed(status, b) => Ok(reply(status, b, true, false)),
            Admitted::Created(b) => {
                if state.nests.is_http() {
                    let s = state.clone();
                    tokio::spawn(async move {
                        dispatch_once(&s).await;
                    });
                }
                Ok(reply(202, b, false, false))
            }
        }
    }
    .await;
    match result {
        Ok(r) => r,
        Err(e) => crate::org_api::api_error(e),
    }
}

fn job_event_in(
    tx: &Transaction<'_>,
    job: &str,
    kind: &str,
    detail: Value,
    now: i64,
) -> GResult<()> {
    tx.execute(
        "INSERT INTO nest_job_events (job_id, kind, detail, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![job, kind, detail.to_string(), now],
    )?;
    // Hardening: the same change as a typed event on the buyer's `/v1/events` stream.
    crate::marketplace_hardening::job_event_in(tx, job, kind, &detail, now)
}

/// `GET /v1/nest-jobs/{job}`
async fn read_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let jid = id.clone();
        let row = state
            .store
            .group_read(move |c| -> GResult<Option<(String, Value)>> {
                Ok(c.query_row(
                    "SELECT account_id, id, address, operation, operation_version, admission,
                            execution, settlement, revision, result, error_code, created_at,
                            updated_at, deadline_at
                       FROM nest_jobs WHERE id = ?1",
                    [&jid],
                    |r| {
                        let result: Option<String> = r.get(9)?;
                        Ok((
                            r.get::<_, String>(0)?,
                            json!({
                                "job_id": r.get::<_, String>(1)?,
                                "to": r.get::<_, String>(2)?,
                                "operation": r.get::<_, String>(3)?,
                                "operation_version": r.get::<_, String>(4)?,
                                "admission": r.get::<_, String>(5)?,
                                "execution": r.get::<_, String>(6)?,
                                "settlement": r.get::<_, String>(7)?,
                                "revision": r.get::<_, i64>(8)?,
                                "effects_outcome_unknown": 0,
                                "result_attachments": [],
                                "result": result
                                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                                    .unwrap_or(Value::Null),
                                "error_code": r.get::<_, Option<String>>(10)?,
                                "created_at": iso(r.get(11)?),
                                "updated_at": iso(r.get(12)?),
                                "deadline_at": iso(r.get(13)?),
                            }),
                        ))
                    },
                )
                .optional()?)
            })
            .await?;
        let f = Facts {
            job_visible: row.as_ref().is_some_and(|(owner, _)| *owner == account),
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::NestJobRead,
            &Resource::NestJob(&id),
            &f,
        )
        .map_err(|d| deny(d, true))?;
        let (_, job) = row.ok_or_else(not_found)?;
        Ok(Json(json!({ "data": job })))
    }
    .await;
    into_response(result)
}

// ---- the dispatcher, postbox side ------------------------------------------------------------------

/// One queued job and what dispatching it needs.
struct Queued {
    id: String,
    listing: String,
    context: String,
    op: String,
    ver: String,
    payload: String,
    release: String,
    security: String,
    image: Option<String>,
    digest: Option<String>,
    attestation: Option<(String, String)>,
    signature: Option<(String, String)>,
    limits: Option<Value>,
    killed: bool,
}

/// Hand queued jobs to nestd's dispatcher (`POST /v1/mp/jobs`). A job whose release is no longer
/// live stops instead. A transport failure leaves the job queued: nestd dedupes by job id, so a
/// retry is never a second job.
pub(crate) async fn dispatch_once(state: &AppState) -> usize {
    let quotas = state.nests.marketplace.quotas;
    let queued = state
        .store
        .group_read(move |c| -> GResult<Vec<Queued>> {
            let mut stmt = c.prepare(
                "SELECT j.id, j.listing_id, j.buyer_context, j.operation, j.operation_version,
                        j.payload, j.release_id, r.security_state, r.image, r.release_digest
                   FROM nest_jobs j JOIN nest_releases r ON r.id = j.release_id
                  WHERE j.execution IN ('queued','reconciling') ORDER BY j.created_at, j.id LIMIT 20",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(Queued {
                        id: r.get(0)?,
                        listing: r.get(1)?,
                        context: r.get(2)?,
                        op: r.get(3)?,
                        ver: r.get(4)?,
                        payload: r.get(5)?,
                        release: r.get(6)?,
                        security: r.get(7)?,
                        image: r.get(8)?,
                        digest: r.get(9)?,
                        attestation: None,
                        signature: None,
                        limits: None,
                        killed: false,
                    })
                })?
                .collect::<rusqlite::Result<Vec<Queued>>>()?;
            let mut out = Vec::new();
            for mut q in rows {
                q.attestation = attestation_in(c, &q.release)?;
                q.signature = crate::marketplace_hardening::signature_in(c, &q.release)?;
                q.limits = crate::marketplace_hardening::limits_in(c, &q.release, quotas)?;
                q.killed = crate::marketplace_hardening::killed_in(c, &q.listing)?;
                out.push(q);
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
    let mut handed = 0;
    for q in queued {
        let Queued {
            id,
            listing,
            context,
            op,
            ver,
            payload,
            release,
            security,
            image,
            digest,
            attestation: att,
            signature,
            limits,
            killed,
        } = q;
        let (next, worker, error): (&str, Option<String>, Option<String>) = if security != "normal"
        {
            (
                "stopped_security",
                None,
                Some("security_quarantined".into()),
            )
        } else if killed {
            ("stopped_security", None, Some("listing_killed".into()))
        } else {
            let (
                Some(image),
                Some(digest),
                Some((statement, statement_digest)),
                Some((key_id, signature)),
            ) = (image, digest, att, signature)
            else {
                continue;
            };
            let job = json!({
                "job_id": id,
                "listing_id": listing,
                "buyer_context": context,
                "operation": op,
                "operation_version": ver,
                "payload": serde_json::from_str::<Value>(&payload).unwrap_or(json!({})),
                "release": {
                    "release_id": release,
                    "image": image,
                    "release_digest": digest,
                    "statement": statement,
                    "statement_digest": statement_digest,
                    "signature": signature,
                    "key_id": key_id,
                },
                "limits": limits,
            });
            match state
                .nests
                .call(reqwest::Method::POST, "/v1/mp/jobs", Some(job))
                .await
            {
                Ok((200, v)) => ("running", v["worker"].as_str().map(str::to_string), None),
                Ok((409, v)) => (
                    "stopped_security",
                    None,
                    Some(
                        v["error"]
                            .as_str()
                            .unwrap_or("release_quarantined")
                            .to_string(),
                    ),
                ),
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
                _ => continue,
            }
        };
        let now = now_unix() as i64;
        let jid = id.clone();
        let stored = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let n = tx.execute(
                    "UPDATE nest_jobs SET execution = ?2, worker = COALESCE(?3, worker),
                        error_code = ?4, revision = revision + 1, updated_at = ?5
                      WHERE id = ?1 AND execution IN ('queued','reconciling')",
                    params![jid, next, worker, error, now],
                )?;
                if n > 0 {
                    job_event_in(
                        tx,
                        &jid,
                        "nest.job.progress",
                        json!({ "execution": next }),
                        now,
                    )?;
                }
                Ok(())
            })
            .await;
        if stored.is_ok() {
            handed += 1;
        }
    }
    handed
}

/// Collect results of running jobs from nestd (`GET /v1/mp/jobs/{job}`).
pub(crate) async fn poll_once(state: &AppState) -> usize {
    let running = state
        .store
        .group_read(|c| -> GResult<Vec<String>> {
            let mut stmt = c.prepare(
                "SELECT id FROM nest_jobs WHERE execution = 'running' ORDER BY updated_at, id LIMIT 50",
            )?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let mut done = 0;
    for id in running {
        let v = match state
            .nests
            .call(reqwest::Method::GET, &format!("/v1/mp/jobs/{id}"), None)
            .await
        {
            Ok((200, v)) => v,
            // The worker host no longer knows a running job (restored ledger, lost host): it is
            // reconciling, and the dispatcher hands it over again (nestd dedupes by job id).
            Ok((404, _)) => {
                let (jid, now) = (id.clone(), now_unix() as i64);
                let _ = state
                    .store
                    .group_tx(move |tx| -> GResult<()> {
                        let n = tx.execute(
                            "UPDATE nest_jobs SET execution = 'reconciling',
                                revision = revision + 1, updated_at = ?2
                              WHERE id = ?1 AND execution = 'running'",
                            params![jid, now],
                        )?;
                        if n > 0 {
                            job_event_in(
                                tx,
                                &jid,
                                "nest.job.reconciling",
                                json!({ "execution": "reconciling" }),
                                now,
                            )?;
                        }
                        Ok(())
                    })
                    .await;
                continue;
            }
            _ => continue,
        };
        let next = match v["state"].as_str() {
            Some("succeeded") => "succeeded",
            Some("failed") => "failed",
            Some("stopped_security") => "stopped_security",
            _ => continue,
        };
        let result = (next == "succeeded").then(|| {
            let r = &v["result"];
            json!({
                "answer": r["answer"],
                "sources": r["sources"],
                "model_used": r["model_used"],
            })
            .to_string()
        });
        let error = (next != "succeeded").then(|| {
            v["result"]["error_code"]
                .as_str()
                .unwrap_or(next)
                .to_string()
        });
        let now = now_unix() as i64;
        let stored = state
            .store
            .group_tx(move |tx| -> GResult<bool> {
                let n = tx.execute(
                    "UPDATE nest_jobs SET execution = ?2, result = ?3, error_code = ?4,
                        revision = revision + 1, updated_at = ?5
                      WHERE id = ?1 AND execution = 'running'",
                    params![id, next, result, error, now],
                )?;
                if n > 0 {
                    let kind = if next == "succeeded" {
                        "nest.job.result_ready"
                    } else {
                        "nest.job.progress"
                    };
                    job_event_in(tx, &id, kind, json!({ "execution": next }), now)?;
                }
                Ok(n > 0)
            })
            .await;
        if stored.unwrap_or(false) {
            done += 1;
        }
    }
    done
}

/// The dispatcher loop, when nestd is configured over HTTP.
pub(crate) fn start_dispatcher(state: AppState) {
    if !state.nests.is_http() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            // Kill-switch stops first: a killed listing's workers stop before any new dispatch.
            crate::marketplace_hardening::reconcile_runtime_stops(&state).await;
            dispatch_once(&state).await;
            poll_once(&state).await;
            crate::marketplace_hardening::source_builds_once(&state).await;
            crate::marketplace_hardening::wake_new_events(&state).await;
        }
    });
}

// ---- staff: review, decision and attestation ---------------------------------------------------------

/// The review case behind a staff route, with the reviewer's roles and stake in it.
struct StaffView {
    case_id: String,
    release_id: String,
    status: String,
    roles: Vec<StaffRole>,
    conflict: store::ReviewConflict,
}

async fn staff_view(state: &AppState, account: &str, review: &str) -> GResult<Option<StaffView>> {
    let (who, rid) = (account.to_string(), review.to_string());
    state
        .store
        .group_read(move |c| -> GResult<Option<StaffView>> {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT release_id, status FROM review_cases WHERE id = ?1",
                    [&rid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let roles = store::live_staff_roles_in(c, &who)?;
            let Some((release_id, status)) = row else {
                return Ok(None);
            };
            let conflict = store::review_conflict_in(c, &who, &release_id)?;
            Ok(Some(StaffView {
                case_id: rid,
                release_id,
                status,
                roles,
                conflict,
            }))
        })
        .await
}

/// Role, step-up, then the conflict rule with the first-party exception. A missing case is
/// answered 404 after the role check, never as a conflict.
async fn staff_gate(
    state: &AppState,
    principal: &Principal,
    account: &str,
    review: &str,
    actions: &[Action],
) -> GResult<StaffView> {
    let view = staff_view(state, account, review).await?;
    let roles = match &view {
        Some(v) => v.roles.clone(),
        None => {
            let who = account.to_string();
            state
                .store
                .group_read(move |c| -> GResult<Vec<StaffRole>> {
                    Ok(store::live_staff_roles_in(c, &who)?)
                })
                .await?
        }
    };
    for action in actions {
        let f = Facts {
            staff_roles: roles.clone(),
            staff_conflict: view.as_ref().is_some_and(|v| v.conflict.conflicted),
            ..facts(state)
        };
        authorize(
            &Actor::Principal(principal),
            *action,
            &Resource::StaffSubject(review),
            &f,
        )
        .map_err(|d| deny(d, false))?;
    }
    view.ok_or_else(not_found)
}

fn stages_in(c: &Connection, case: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = c.prepare(
        "SELECT id, stage, decision, reviewer, reason, evidence_digest, first_party, created_at
           FROM review_decisions WHERE case_id = ?1 ORDER BY created_at, rowid",
    )?;
    let rows = stmt
        .query_map([case], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "stage": r.get::<_, String>(1)?,
                "decision": r.get::<_, String>(2)?,
                "reviewer": r.get::<_, String>(3)?,
                "reason": r.get::<_, String>(4)?,
                "evidence_digest": r.get::<_, Option<String>>(5)?,
                "first_party": r.get::<_, i64>(6)? == 1,
                "created_at": iso(r.get(7)?),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `GET /v1/staff/reviews/{review}`
async fn staff_read_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(review): Path<String>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let view = staff_gate(&state, &principal, &account, &review, &[Action::NestReview]).await?;
        let (case, release) = (view.case_id.clone(), view.release_id.clone());
        let body = state
            .store
            .group_read(move |c| -> GResult<Value> {
                let (publisher, version, digest, image, sbom_digest, source_digest, security) = c
                    .query_row(
                    "SELECT publisher_id, version, release_digest, image, sbom_digest,
                                source_digest, security_state FROM nest_releases WHERE id = ?1",
                    [&release],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, String>(6)?,
                        ))
                    },
                )?;
                Ok(json!({
                    "release_id": release,
                    "publisher_id": publisher,
                    "version": version,
                    "release_digest": digest,
                    "image": image,
                    "sbom_digest": sbom_digest,
                    "source_digest": source_digest,
                    "security_state": security,
                    "stages": stages_in(c, &case)?,
                }))
            })
            .await?;
        let mut data = body;
        data["id"] = json!(view.case_id);
        data["status"] = json!(view.status);
        data["first_party"] = json!(view.conflict.first_party);
        Ok(Json(json!({ "data": data })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionReq {
    decision: String,
    reason: String,
    /// The digest the reviewer is approving, so a decision is never about a release other than
    /// the one they looked at.
    release_digest: String,
}

/// The attestation statement: canonical JSON over the release digest and the decision.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attestation_statement(
    release_id: &str,
    release_digest: &str,
    image: &str,
    case_id: &str,
    decision_id: &str,
    attester: &str,
    first_party: bool,
    stages: &Value,
    at: i64,
    key_id: &str,
) -> String {
    canonical_json(&json!({
        "key_id": key_id,
        "type": STATEMENT_TYPE,
        "release_id": release_id,
        "release_digest": release_digest,
        "image": image,
        "image_digest": image.split_once('@').map(|(_, d)| d).unwrap_or(""),
        "review_id": case_id,
        "decision_id": decision_id,
        "decision": "approved",
        "attester": attester,
        "first_party": first_party,
        "stages": stages,
        "attested_at": iso(at),
    }))
}

/// `POST /v1/staff/reviews/{review}/decisions` — a staff decision; an approval also writes the
/// attestation over the release digest and publishes the release's first-party listing.
async fn staff_decide(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let req: DecisionReq = crate::marketplace::parse(body)?;
        if !matches!(
            req.decision.as_str(),
            "approved" | "rejected" | "changes_requested"
        ) {
            return Err(GErr::bad(
                "invalid_request",
                "decision: approved, rejected or changes_requested",
            ));
        }
        let reason = req.reason.trim().to_string();
        if reason.is_empty() || reason.chars().count() > 2000 {
            return Err(GErr::bad("invalid_request", "reason: 1-2000 characters"));
        }
        let mut actions = vec![Action::NestReview];
        if req.decision == "approved" {
            actions.push(Action::NestAttest);
        }
        let view = staff_gate(&state, &principal, &account, &review, &actions).await?;
        // Hardening: an approval is only attested with a signature (503 without a key).
        let signer = if req.decision == "approved" {
            Some(state.nests.marketplace.signer()?)
        } else {
            None
        };
        let platform_owner = state.platform_owner.as_deref().map(str::to_string);
        let now = now_unix() as i64;
        let decision_id = format!("dec_{}", crate::groups::rand_b32(16));
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "marketplace.review.decision",
            Some(format!("release:{}", view.release_id)),
            json!({ "decision": req.decision, "first_party": view.conflict.first_party }),
            ip_of(&client_ip),
            user_agent(&headers),
            now as u64,
        );
        let first_party = view.conflict.first_party;
        let (who, did) = (account.clone(), decision_id.clone());
        let out = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let status: String = tx.query_row(
                    "SELECT status FROM review_cases WHERE id = ?1",
                    [&view.case_id],
                    |r| r.get(0),
                )?;
                if status != "open" {
                    return Err(conflict_code("review_closed", "this review is decided or blocked"));
                }
                let (digest, image, security, manifest): (Option<String>, Option<String>, String, Option<String>) =
                    tx.query_row(
                        "SELECT release_digest, image, security_state, manifest FROM nest_releases WHERE id = ?1",
                        [&view.release_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                match security.as_str() {
                    "revoked" => return Err(conflict_code("release_revoked", "this release is revoked")),
                    "quarantined" => {
                        return Err(conflict_code("security_quarantined", "this release is quarantined"))
                    }
                    _ => {}
                }
                let (Some(digest), Some(image)) = (digest, image) else {
                    return Err(conflict_code("review_required", "this release has no build record"));
                };
                if digest != req.release_digest {
                    return Err(conflict_code(
                        "release_digest_mismatch",
                        "the release digest is not the one under review",
                    ));
                }
                // The evidence an approval rests on: the latest record of each automated stage.
                let mut stages = Map::new();
                let mut missing = Vec::new();
                for (stage, wanted) in [
                    ("intake", "passed"),
                    ("deterministic_scan", "passed"),
                    ("behavioral_test", "passed"),
                    ("ai_analysis", "advisory"),
                ] {
                    let latest: Option<(String, Option<String>)> = tx
                        .query_row(
                            "SELECT decision, evidence_digest FROM review_decisions
                              WHERE case_id = ?1 AND stage = ?2 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                            params![view.case_id, stage],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    match latest {
                        Some((d, ev)) if d == wanted => {
                            stages.insert(stage.to_string(), json!(ev));
                        }
                        _ => missing.push(stage),
                    }
                }
                if req.decision == "approved" && !missing.is_empty() {
                    return Err(conflict_code(
                        "stages_incomplete",
                        "every analysis stage must pass (the AI review is advisory) before approval",
                    )
                    .with("stages", json!(missing)));
                }
                tx.execute(
                    "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at,
                        stage, first_party)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'human_review', ?7)",
                    params![did, view.case_id, who, req.decision, reason, now, first_party as i64],
                )?;
                let mut attestation = Value::Null;
                if let (true, Some((key, key_id))) = (req.decision == "approved", signer.as_ref()) {
                    let stages = Value::Object(stages);
                    let statement = attestation_statement(
                        &view.release_id,
                        &digest,
                        &image,
                        &view.case_id,
                        &did,
                        &who,
                        first_party,
                        &stages,
                        now,
                        key_id,
                    );
                    let statement_digest = digest_of(statement.as_bytes());
                    let aid = format!("att_{}", crate::groups::rand_b32(16));
                    tx.execute(
                        "INSERT INTO release_attestations (id, release_id, case_id, attester,
                            statement_digest, created_at, release_digest, decision_id, statement, first_party)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            aid, view.release_id, view.case_id, who, statement_digest, now, digest,
                            did, statement, first_party as i64
                        ],
                    )?;
                    let signature = crate::marketplace_hardening::sign(key, &statement);
                    crate::marketplace_hardening::record_signature_in(tx, &aid, key_id, &signature, now)?;
                    tx.execute(
                        "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason,
                            created_at, stage, evidence_digest)
                         VALUES (?1, ?2, ?3, 'passed', 'attested', ?4, 'attestation', ?5)",
                        params![
                            format!("dec_{}", crate::groups::rand_b32(16)),
                            view.case_id,
                            who,
                            now,
                            statement_digest
                        ],
                    )?;
                    tx.execute(
                        "UPDATE nest_releases SET status = 'approved' WHERE id = ?1",
                        [&view.release_id],
                    )?;
                    // A first-party release publishes its listing and its operations with it.
                    let m: Value = manifest
                        .as_deref()
                        .and_then(|t| serde_json::from_str(t).ok())
                        .unwrap_or(Value::Null);
                    if let Some(address) = m["listing_address"].as_str() {
                        let listing: Option<String> = tx
                            .query_row(
                                "SELECT id FROM nest_listings WHERE address = ?1",
                                [address],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if let Some(listing) = listing {
                            tx.execute(
                                "UPDATE nest_listings SET release_id = ?2, status = 'published',
                                    updated_at = ?3 WHERE id = ?1",
                                params![listing, view.release_id, now],
                            )?;
                            tx.execute(
                                "DELETE FROM nest_operations WHERE listing_id = ?1",
                                [&listing],
                            )?;
                            for op in m["operations"].as_array().cloned().unwrap_or_default() {
                                tx.execute(
                                    "INSERT INTO nest_operations (listing_id, operation,
                                        operation_version, input_schema)
                                     VALUES (?1, ?2, ?3, ?4)",
                                    params![
                                        listing,
                                        op["name"].as_str().unwrap_or(""),
                                        op["version"].as_str().unwrap_or(""),
                                        json!({ "fields": op["fields"] }).to_string()
                                    ],
                                )?;
                            }
                        }
                    }
                    attestation = json!({
                        "id": aid,
                        "statement_digest": statement_digest,
                        "key_id": key_id,
                        "signature": signature,
                    });
                } else if req.decision == "rejected" {
                    tx.execute(
                        "UPDATE nest_releases SET status = 'rejected' WHERE id = ?1",
                        [&view.release_id],
                    )?;
                }
                tx.execute(
                    "UPDATE review_cases SET status = 'decided', updated_at = ?2 WHERE id = ?1",
                    params![view.case_id, now],
                )?;
                for kind in ["decision", "completed"] {
                    crate::marketplace_hardening::review_event_in(
                        tx,
                        &view.case_id,
                        kind,
                        json!({ "decision": req.decision }),
                        platform_owner.as_deref(),
                        now,
                    )?;
                }
                crate::store::audit::append_in(tx, &audit)?;
                Ok(json!({
                    "decision_id": did,
                    "decision": req.decision,
                    "first_party": first_party,
                    "attestation": attestation,
                }))
            })
            .await?;
        crate::marketplace_hardening::wake_new_events(&state).await;
        Ok((StatusCode::CREATED, Json(json!({ "data": out }))))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecurityReq {
    action: String,
    reason: String,
}

/// `POST /v1/staff/releases/{release}/security` — quarantine, lift a quarantine, or revoke.
/// Running workers stop, new jobs are refused, an event is recorded. Revocation is permanent.
async fn staff_security(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(release): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let req: SecurityReq = crate::marketplace::parse(body)?;
        let (next, event) = match req.action.as_str() {
            "quarantine" => ("quarantined", "quarantined"),
            "lift" => ("normal", "released"),
            "revoke" => ("revoked", "revoked"),
            _ => {
                return Err(GErr::bad(
                    "invalid_request",
                    "action: quarantine, lift or revoke",
                ))
            }
        };
        let reason = req.reason.trim().to_string();
        if reason.is_empty() || reason.chars().count() > 2000 {
            return Err(GErr::bad("invalid_request", "reason: 1-2000 characters"));
        }
        let (who, rel) = (account.clone(), release.clone());
        let (roles, conflict, exists) = state
            .store
            .group_read(move |c| -> GResult<(Vec<StaffRole>, store::ReviewConflict, bool)> {
                let exists: bool = c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM nest_releases WHERE id = ?1)",
                    [&rel],
                    |r| r.get(0),
                )?;
                Ok((
                    store::live_staff_roles_in(c, &who)?,
                    store::review_conflict_in(c, &who, &rel)?,
                    exists,
                ))
            })
            .await?;
        let f = Facts {
            staff_roles: roles,
            staff_conflict: exists && conflict.conflicted,
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::NestQuarantine,
            &Resource::StaffSubject(&release),
            &f,
        )
        .map_err(|d| deny(d, false))?;
        if !exists {
            return Err(not_found());
        }
        let now = now_unix() as i64;
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            &format!("marketplace.release.{event}"),
            Some(format!("release:{release}")),
            json!({ "action": req.action }),
            ip_of(&client_ip),
            user_agent(&headers),
            now as u64,
        );
        let (rel, who) = (release.clone(), account.clone());
        let platform_owner = state.platform_owner.as_deref().map(str::to_string);
        let stopped = state
            .store
            .group_tx(move |tx| -> GResult<i64> {
                let current: String = tx.query_row(
                    "SELECT security_state FROM nest_releases WHERE id = ?1",
                    [&rel],
                    |r| r.get(0),
                )?;
                match (current.as_str(), next) {
                    ("revoked", _) => {
                        return Err(conflict_code("release_revoked", "a revoked release stays revoked"))
                    }
                    ("normal", "normal") => {
                        return Err(conflict_code("invalid_request", "this release is not quarantined"))
                    }
                    _ => {}
                }
                if next == "revoked" {
                    tx.execute(
                        "UPDATE nest_releases SET security_state = 'revoked', status = 'revoked' WHERE id = ?1",
                        [&rel],
                    )?;
                    tx.execute(
                        "UPDATE nest_listings SET status = 'suspended', updated_at = ?2 WHERE release_id = ?1",
                        params![rel, now],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE nest_releases SET security_state = ?2 WHERE id = ?1",
                        params![rel, next],
                    )?;
                }
                tx.execute(
                    "INSERT INTO release_security_events (id, release_id, action, actor, reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        format!("sev_{}", crate::groups::rand_b32(16)),
                        rel,
                        event,
                        who,
                        reason,
                        now
                    ],
                )?;
                let mut stopped = 0i64;
                if next != "normal" {
                    let jobs: Vec<String> = {
                        let mut stmt = tx.prepare(
                            "SELECT id FROM nest_jobs WHERE release_id = ?1
                               AND execution IN ('queued','running','waiting_input')",
                        )?;
                        let rows = stmt
                            .query_map([&rel], |r| r.get(0))?
                            .collect::<rusqlite::Result<Vec<String>>>()?;
                        rows
                    };
                    for job in jobs {
                        tx.execute(
                            "UPDATE nest_jobs SET execution = 'stopped_security', error_code = ?2,
                                revision = revision + 1, updated_at = ?3 WHERE id = ?1",
                            params![job, format!("release_{event}"), now],
                        )?;
                        job_event_in(
                            tx,
                            &job,
                            "nest.security.stopped",
                            json!({ "execution": "stopped_security", "release": rel }),
                            now,
                        )?;
                        stopped += 1;
                    }
                }
                if next != "normal" {
                    crate::marketplace_hardening::security_event_in(
                        tx,
                        next,
                        Some(&rel),
                        None,
                        platform_owner.as_deref(),
                        now,
                    )?;
                }
                crate::store::audit::append_in(tx, &audit)?;
                Ok(stopped)
            })
            .await?;
        crate::marketplace_hardening::wake_new_events(&state).await;
        // Running workers stop at once on the worker host. The postbox has already refused new
        // jobs; a host that cannot be reached is told again by the dispatcher (it refuses work for
        // this release because the postbox never sends any).
        let runtime = state
            .nests
            .call(
                reqwest::Method::POST,
                &format!("/v1/mp/releases/{release}/quarantine"),
                Some(json!({ "revoke": next == "revoked", "lift": next == "normal" })),
            )
            .await;
        let workers_stopped = matches!(runtime, Ok((200, _)));
        Ok((
            StatusCode::OK,
            Json(json!({ "data": {
                "release_id": release,
                "security_state": next,
                "jobs_stopped": stopped,
                "runtime_updated": workers_stopped,
            }})),
        ))
    }
    .await;
    into_response(result)
}

// ---- internal: build plane and analysis plane ------------------------------------------------------

/// The ops service token, as for every internal route.
fn service_gate(state: &AppState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    crate::internal_api::authorize_service(
        state,
        headers,
        crate::internal_api::Endpoint::Marketplace,
        Action::ManageNests,
    )
    .map(|_| ())
}

/// The internal routes need the `marketplace` flag on for the platform owner's account.
async fn flag_gate(state: &AppState) -> GResult<()> {
    let owner = state.platform_owner.as_deref();
    if owner.is_none()
        || !crate::filtering::feature_on(state, owner, capabilities::MARKETPLACE).await
    {
        return Err(feature_off());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildReq {
    manifest: Value,
    manifest_digest: String,
    source_digest: String,
    vendor_digest: String,
    sbom: String,
    sbom_digest: String,
    image: String,
    image_digest: String,
    release_digest: String,
    isolation: Value,
}

fn check_build(b: &BuildReq) -> GResult<()> {
    let bad = |m: &str| GErr::bad("invalid_request", m.to_string());
    for d in [
        &b.manifest_digest,
        &b.source_digest,
        &b.vendor_digest,
        &b.sbom_digest,
        &b.image_digest,
        &b.release_digest,
    ] {
        if !sha256_ok(d) {
            return Err(bad("every digest is sha256:<64 hex>"));
        }
    }
    if digest_of(b.sbom.as_bytes()) != b.sbom_digest {
        return Err(bad("sbom_digest does not match the SBOM"));
    }
    let sbom: Value = serde_json::from_str(&b.sbom).map_err(|_| bad("the SBOM is not JSON"))?;
    if sbom["bomFormat"] != "CycloneDX" {
        return Err(bad("the SBOM is CycloneDX JSON"));
    }
    if b.image.split_once('@').map(|(_, d)| d) != Some(b.image_digest.as_str())
        || b.image.starts_with('-')
    {
        return Err(bad("image is name@<image_digest>"));
    }
    if release_digest(
        &b.source_digest,
        &b.vendor_digest,
        &b.manifest_digest,
        &b.sbom_digest,
        &b.image_digest,
    ) != b.release_digest
    {
        return Err(bad("release_digest does not cover these digests"));
    }
    let m = &b.manifest;
    let slug = m["name"].as_str().unwrap_or("");
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(bad("manifest.name"));
    }
    if !m["listing_address"]
        .as_str()
        .is_some_and(|a| a.starts_with('/') && a.len() <= 128)
    {
        return Err(bad("manifest.listing_address"));
    }
    let ops = m["operations"].as_array().cloned().unwrap_or_default();
    if ops.is_empty()
        || !ops.iter().all(|o| {
            o["name"].as_str().is_some_and(operation_ok)
                && o["version"].as_str().is_some_and(version_ok)
                && o["fields"].as_array().is_some_and(|f| !f.is_empty())
        })
    {
        return Err(bad("manifest.operations"));
    }
    for k in ["version", "title", "summary"] {
        if !m[k]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 200)
        {
            return Err(bad(&format!("manifest.{k}")));
        }
    }
    Ok(())
}

/// `POST /internal/v1/marketplace/builds` — the build plane records a first-party release. M1 has
/// no external publishers: every build belongs to the platform publisher.
async fn register_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(r) = service_gate(&state, &headers) {
        return *r;
    }
    let result = async {
        flag_gate(&state).await?;
        let b: BuildReq = crate::marketplace::parse(body)?;
        check_build(&b)?;
        let now = now_unix() as i64;
        let platform_owner = state.platform_owner.as_deref().map(str::to_string);
        let out = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                register_build_in(tx, &b, None, platform_owner.as_deref(), now)
            })
            .await?;
        crate::marketplace_hardening::wake_new_events(&state).await;
        Ok((StatusCode::CREATED, Json(json!({ "data": out }))))
    }
    .await;
    into_response(result)
}

/// A checked build record becomes a release, its listing (new or the publisher's own), a review
/// case and the passed intake stage. `publisher`: a source build's publisher; `None` is the
/// platform publisher (every M1 build).
fn register_build_in(
    tx: &Transaction<'_>,
    b: &BuildReq,
    publisher: Option<&str>,
    platform_owner: Option<&str>,
    now: i64,
) -> GResult<Value> {
    let publisher = match publisher {
        Some(p) => p.to_string(),
        None => platform_publisher_in(tx, now)?,
    };
    let slug = b.manifest["name"].as_str().unwrap_or("").to_string();
    let version = b.manifest["version"].as_str().unwrap_or("").to_string();
    let template: String = match tx
        .query_row(
            "SELECT id FROM nest_templates WHERE publisher_id = ?1 AND slug = ?2",
            params![publisher, slug],
            |r| r.get(0),
        )
        .optional()?
    {
        Some(t) => t,
        None => {
            let t = format!("tpl_{}", crate::groups::rand_b32(16));
            tx.execute(
                "INSERT INTO nest_templates (id, publisher_id, slug, name, created_by, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    t,
                    publisher,
                    slug,
                    b.manifest["title"].as_str().unwrap_or(""),
                    BUILD_PLANE,
                    now
                ],
            )?;
            t
        }
    };
    let exists: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM nest_releases WHERE template_id = ?1 AND version = ?2)",
        params![template, version],
        |r| r.get(0),
    )?;
    if exists {
        return Err(conflict_code(
            "release_exists",
            "this version is already recorded",
        ));
    }
    let release = format!("rel_{}", crate::groups::rand_b32(16));
    tx.execute(
                    "INSERT INTO nest_releases (id, template_id, publisher_id, version, image_digest,
                        status, created_by, created_at, release_digest, source_digest, vendor_digest,
                        manifest_digest, sbom_digest, sbom, manifest, image, build_isolation)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'submitted', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                    params![
                        release, template, publisher, version, b.image_digest, BUILD_PLANE, now,
                        b.release_digest, b.source_digest, b.vendor_digest, b.manifest_digest,
                        b.sbom_digest, b.sbom, b.manifest.to_string(), b.image, b.isolation.to_string()
                    ],
                )?;
    let address = b.manifest["listing_address"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let listing: String = match tx
        .query_row(
            "SELECT id, publisher_id FROM nest_listings WHERE address = ?1",
            [&address],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        Some((id, owner)) if owner == publisher => id,
        Some(_) => {
            return Err(conflict_code(
                "listing_address_taken",
                "another publisher holds this address",
            ))
        }
        None => {
            let id = format!("lst_{}", crate::groups::rand_b32(16));
            tx.execute(
                "INSERT INTO nest_listings (id, publisher_id, release_id, title, summary,
                                visibility, status, created_at, updated_at, address)
                             VALUES (?1, ?2, ?3, ?4, ?5, 'public', 'draft', ?6, ?6, ?7)",
                params![
                    id,
                    publisher,
                    release,
                    b.manifest["title"].as_str().unwrap_or(""),
                    b.manifest["summary"].as_str().unwrap_or(""),
                    now,
                    address
                ],
            )?;
            id
        }
    };
    let previous: Option<String> = tx
                    .query_row(
                        "SELECT c.id FROM review_cases c JOIN nest_releases r ON r.id = c.release_id
                          WHERE r.template_id = ?1 AND c.id IS NOT NULL ORDER BY c.created_at DESC LIMIT 1",
                        [&template],
                        |r| r.get(0),
                    )
                    .optional()?;
    let case = format!("rvw_{}", crate::groups::rand_b32(16));
    tx.execute(
                    "INSERT INTO review_cases (id, release_id, publisher_id, status, previous_id, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?5)",
                    params![case, release, publisher, previous, now],
                )?;
    let evidence = canonical_json(&json!({
        "stage": "intake",
        "decision": "passed",
        "isolation": b.isolation,
        "release_digest": b.release_digest,
        "source_digest": b.source_digest,
        "vendor_digest": b.vendor_digest,
        "sbom_digest": b.sbom_digest,
        "image_digest": b.image_digest,
    }));
    tx.execute(
                    "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at,
                        stage, evidence, evidence_digest)
                     VALUES (?1, ?2, ?3, 'passed', 'built from the pinned source tree', ?4, 'intake', ?5, ?6)",
                    params![
                        format!("dec_{}", crate::groups::rand_b32(16)),
                        case,
                        BUILD_PLANE,
                        now,
                        evidence,
                        digest_of(evidence.as_bytes())
                    ],
                )?;
    crate::marketplace_hardening::review_event_in(
        tx,
        &case,
        "stage_changed",
        json!({ "stage": "intake", "decision": "passed" }),
        platform_owner,
        now,
    )?;
    Ok(
        json!({ "release_id": release, "review_id": case, "listing_id": listing, "publisher_id": publisher }),
    )
}

/// The platform publisher, made on first use. It is approved by construction: it is Pigeonpost.
fn platform_publisher_in(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<String> {
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM publisher_profiles WHERE owner_kind = 'platform' AND owner_id = ?1
               AND status = 'approved'",
            [PLATFORM_OWNER_ID],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let id = format!("pub_{}", crate::groups::rand_b32(16));
    tx.execute(
        "INSERT INTO publisher_profiles (id, owner_kind, owner_id, display_name, status, applied_by,
            decided_by, decided_at, created_at, updated_at)
         VALUES (?1, 'platform', ?2, 'Pigeonpost', 'approved', ?3, ?3, ?4, ?4, ?4)",
        params![id, PLATFORM_OWNER_ID, BUILD_PLANE, now],
    )?;
    Ok(id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StageReq {
    stage: String,
    decision: String,
    reason: String,
    /// The evidence document exactly as hashed (`StageReport::evidence_json`).
    evidence: String,
    evidence_digest: String,
}

/// `POST /internal/v1/marketplace/reviews/{review}/stages` — one analysis stage's outcome.
async fn record_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(review): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(r) = service_gate(&state, &headers) {
        return *r;
    }
    let result = async {
        flag_gate(&state).await?;
        let s: StageReq = crate::marketplace::parse(body)?;
        check_stage(&s)?;
        let now = now_unix() as i64;
        let platform_owner = state.platform_owner.as_deref().map(str::to_string);
        let id = state
            .store
            .group_tx(move |tx| -> GResult<String> {
                record_stage_in(tx, &review, &s, platform_owner.as_deref(), now)
            })
            .await?;
        crate::marketplace_hardening::wake_new_events(&state).await;
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": { "decision_id": id } })),
        ))
    }
    .await;
    into_response(result)
}

/// What a stage report must be before it is recorded: a stage the analysis plane may record, a
/// decision that stage may take, evidence that is its own digest and names that stage.
fn check_stage(s: &StageReq) -> GResult<()> {
    let bad = |m: &str| GErr::bad("invalid_request", m.to_string());
    let wanted = match s.stage.as_str() {
        "deterministic_scan" | "behavioral_test" => ["passed", "failed"].as_slice(),
        // The AI review never decides (R-64, R-65).
        "ai_analysis" => ["advisory"].as_slice(),
        _ => {
            return Err(bad(
                "stage: deterministic_scan, behavioral_test or ai_analysis",
            ))
        }
    };
    if !wanted.contains(&s.decision.as_str()) {
        return Err(bad("this stage cannot record that decision"));
    }
    if digest_of(s.evidence.as_bytes()) != s.evidence_digest {
        return Err(bad("evidence_digest does not match the evidence"));
    }
    let ev: Value = serde_json::from_str(&s.evidence).map_err(|_| bad("evidence is JSON"))?;
    if ev["stage"] != s.stage.as_str() || ev["decision"] != s.decision.as_str() {
        return Err(bad("the evidence is for another stage or decision"));
    }
    if s.stage == "ai_analysis" && ev["evidence"]["model"] != AI_REVIEW_MODEL {
        return Err(bad("the AI review runs on the self-hosted model only"));
    }
    Ok(())
}

/// Record a checked stage on an open review; a failed stage blocks it. Emits
/// `nest.review.stage_changed` (and `nest.review.blocked`).
fn record_stage_in(
    tx: &Transaction<'_>,
    review: &str,
    s: &StageReq,
    platform_owner: Option<&str>,
    now: i64,
) -> GResult<String> {
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM review_cases WHERE id = ?1",
            [review],
            |r| r.get(0),
        )
        .optional()?;
    match status.as_deref() {
        None => return Err(not_found()),
        Some("open") => {}
        Some(_) => {
            return Err(conflict_code(
                "review_closed",
                "this review is decided or blocked",
            ))
        }
    }
    let reason = s.reason.chars().take(2000).collect::<String>();
    let id = format!("dec_{}", crate::groups::rand_b32(16));
    tx.execute(
        "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at,
            stage, evidence, evidence_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            review,
            ANALYSIS_PLANE,
            s.decision,
            reason,
            now,
            s.stage,
            s.evidence,
            s.evidence_digest
        ],
    )?;
    crate::marketplace_hardening::review_event_in(
        tx,
        review,
        "stage_changed",
        json!({ "stage": s.stage, "decision": s.decision }),
        platform_owner,
        now,
    )?;
    if s.decision == "failed" {
        tx.execute(
            "UPDATE review_cases SET status = 'blocked', updated_at = ?2 WHERE id = ?1",
            params![review, now],
        )?;
        crate::marketplace_hardening::review_event_in(
            tx,
            review,
            "blocked",
            json!({ "stage": s.stage }),
            platform_owner,
            now,
        )?;
    }
    Ok(id)
}

/// A finished source build from the remote build plane (`marketplace_hardening`): its record is
/// checked as `POST /internal/v1/marketplace/builds` checks one, registered for `publisher`, and
/// its analysis stages are recorded on the new review, all in one transaction. Returns
/// `(release_id, review_id)`.
pub(crate) async fn register_source_build(
    state: &AppState,
    build_id: &str,
    publisher: &str,
    result: &Value,
) -> GResult<(String, String)> {
    let b: BuildReq = serde_json::from_value(result["record"].clone())
        .map_err(|e| GErr::bad("invalid_build", e.to_string()))?;
    check_build(&b)?;
    let mut stages = Vec::new();
    for st in result["stages"].as_array().cloned().unwrap_or_default() {
        let s: StageReq =
            serde_json::from_value(st).map_err(|e| GErr::bad("invalid_build", e.to_string()))?;
        check_stage(&s)?;
        stages.push(s);
    }
    if result["reproducibility"]["equal"] != true {
        return Err(GErr::bad(
            "not_reproducible",
            "the build plane did not confirm a reproducible build",
        ));
    }
    let now = now_unix() as i64;
    let platform_owner = state.platform_owner.as_deref().map(str::to_string);
    let (p, bid) = (publisher.to_string(), build_id.to_string());
    state
        .store
        .group_tx(move |tx| -> GResult<(String, String)> {
            let out = register_build_in(tx, &b, Some(&p), platform_owner.as_deref(), now)?;
            let release = out["release_id"].as_str().unwrap_or_default().to_string();
            let review = out["review_id"].as_str().unwrap_or_default().to_string();
            for s in &stages {
                record_stage_in(tx, &review, s, platform_owner.as_deref(), now)?;
            }
            tracing::info!(build = %bid, %release, "source build registered for review");
            Ok((release, review))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_digest_matches_the_nests_test_vector() {
        // The same vector is asserted in `pigeonpost_nests::release`.
        assert_eq!(
            release_digest("sha256:01", "sha256:02", "sha256:03", "sha256:04", "sha256:05"),
            digest_of(
                b"pigeonpost-nest-release-v1\nsource=sha256:01\nvendor=sha256:02\nmanifest=sha256:03\nsbom=sha256:04\nimage=sha256:05\n"
            )
        );
    }

    #[test]
    fn strict_json_refuses_duplicates_depth_and_size() {
        assert!(strict_json(br#"{"a":1,"b":[1,2,{"c":null}]}"#).is_ok());
        let dup = strict_json(br#"{"a":1,"a":2}"#).unwrap_err();
        assert_eq!(dup.code, "invalid_json");
        let nested = strict_json(br#"{"x":{"a":1,"a":2}}"#).unwrap_err();
        assert_eq!(nested.code, "invalid_json");
        let deep = format!("{}{}", "[".repeat(33), "]".repeat(33));
        assert_eq!(
            strict_json(deep.as_bytes()).unwrap_err().code,
            "payload_too_large"
        );
        let ok = format!("{}{}", "[".repeat(32), "]".repeat(32));
        assert!(strict_json(ok.as_bytes()).is_ok());
        let big = format!("{{\"a\":\"{}\"}}", "x".repeat(MAX_JOB_BYTES));
        assert_eq!(
            strict_json(big.as_bytes()).unwrap_err().code,
            "payload_too_large"
        );
        assert_eq!(
            strict_json(b"{\"a\":1} trailing").unwrap_err().code,
            "invalid_json"
        );
    }

    #[test]
    fn references_carry_256_bits_and_match_the_contract_pattern() {
        let r = new_reference();
        let body = r.strip_prefix("payauth_").unwrap();
        assert_eq!(body.len(), 43);
        assert!(body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(new_reference(), r);
    }

    #[test]
    fn payloads_follow_the_operation_fields() {
        let schema = json!({ "fields": [{"name": "question", "max_chars": 10, "required": true}] })
            .to_string();
        let ok = |v: Value| validate_payload(&schema, v.as_object().unwrap());
        assert!(ok(json!({"question": "why?"})).is_ok());
        for bad in [
            json!({}),
            json!({"question": ""}),
            json!({"question": "x".repeat(11)}),
            json!({"question": 3}),
            json!({"question": "ok", "extra": "no"}),
        ] {
            assert_eq!(
                ok(bad.clone()).unwrap_err().code,
                "invalid_operation_payload",
                "{bad}"
            );
        }
    }

    #[test]
    fn buyer_contexts_are_per_listing_and_account_and_safe_names() {
        let a = buyer_context("lst_1", "acct_a");
        assert_ne!(a, buyer_context("lst_1", "acct_b"));
        assert_ne!(a, buyer_context("lst_2", "acct_a"));
        assert_eq!(a, buyer_context("lst_1", "acct_a"));
        assert!(
            a.len() <= 64
                && a.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
    }
}
