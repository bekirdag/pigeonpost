//! Meeting summaries (SDS §14, docs/meeting-summaries.md), postbox side.
//!
//! The postbox is the record of *who may receive* a meeting brief, and the store of the brief as
//! ciphertext. It never handles audio, a transcript or a brief in plaintext:
//!
//! - the calls service opens a meeting for an eligible host (`POST /internal/v1/meetings`) and
//!   records each member's consent answer (`…/attendees`), including the public notes key of the
//!   device that consented;
//! - the meeting-intel service asks for the consenting attendees' notes keys (`…/recipients`) and
//!   stores the finished brief (`…/brief`): one AES-256-GCM ciphertext plus the content key
//!   wrapped for each of those keys;
//! - members list and fetch only the briefs of meetings they consented to (`/v1/meetings…`), and
//!   decrypt them on a device that holds one of their notes keys.
//!
//! Audit rows carry ids, counts and consent outcomes only.

use crate::audit::ActorKind;
use crate::capabilities::{self, principal_account};
use crate::groups::{GErr, GResult};
use crate::internal_api::{authorize_service, Endpoint};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::principal::Principal;
use crate::{bearer, now_unix, principal_for_token, AppState};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

/// How long a brief (and its consent records) is kept, unless the deployment says otherwise.
pub(crate) const DEFAULT_RETENTION_DAYS: u64 = 90;
/// A meeting that never produced a brief is closed as failed after this long.
pub(crate) const PENDING_SECS: u64 = 26 * 3_600;
/// Notes keys one account may register (one per device or browser).
pub(crate) const MAX_KEYS: usize = 10;
/// The largest brief ciphertext accepted, as base64 text.
pub(crate) const MAX_CIPHERTEXT_CHARS: usize = 3 * 1024 * 1024;
/// Attendees one meeting may record (calls are capped at 30 people).
pub(crate) const MAX_ATTENDEES: usize = 40;

fn retention_secs() -> u64 {
    std::env::var("POSTBOX_MEETING_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS)
        .clamp(1, 365)
        * 86_400
}

// ---- routers -----------------------------------------------------------------------------------

/// Member routes (`/v1/meetings…`).
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/meetings", get(list))
        .route("/v1/meetings/keys", get(list_keys))
        .route(
            "/v1/meetings/keys/{key_id}",
            put(put_key).delete(delete_key),
        )
        .route("/v1/meetings/{id}", get(fetch).delete(remove))
}

/// Internal routes (`/internal/v1/meetings…`), merged into the internal listener's router.
pub(crate) fn internal_router() -> Router<AppState> {
    Router::new()
        .route("/internal/v1/meetings", post(open))
        .route("/internal/v1/meetings/{id}/attendees", post(attendee))
        .route("/internal/v1/meetings/{id}/end", post(end))
        .route("/internal/v1/meetings/{id}/recipients", get(recipients))
        .route(
            "/internal/v1/meetings/{id}/brief",
            post(store_brief).layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
}

// ---- validation helpers ------------------------------------------------------------------------

fn valid_meeting_id(id: &str) -> bool {
    id.len() == 30
        && id.starts_with("mtg_")
        && id[4..]
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn valid_key_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Standard base64 of exactly `len` bytes (or any length up to `max` when `len` is 0).
fn b64_of(field: &'static str, text: &str, len: usize) -> GResult<Vec<u8>> {
    let bytes = crate::groups::unb64(field, text)?;
    if len != 0 && bytes.len() != len {
        return Err(GErr::bad(
            "bad_request",
            format!("{field} has the wrong length"),
        ));
    }
    Ok(bytes)
}

/// A P-256 public key as base64 of its 65-byte uncompressed SEC1 point, on the curve.
fn valid_p256(field: &'static str, text: &str) -> GResult<()> {
    let raw = b64_of(field, text, 65)?;
    if raw[0] != 4 || p256::PublicKey::from_sec1_bytes(&raw).is_err() {
        return Err(GErr::bad(
            "bad_request",
            format!("{field} is not an uncompressed P-256 public key"),
        ));
    }
    Ok(())
}

fn not_found() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "not_found", "no such meeting")
}

fn feature_off() -> GErr {
    GErr::new(
        StatusCode::NOT_FOUND,
        "feature_disabled",
        "meeting summaries are not enabled for this account",
    )
}

fn service(
    state: &AppState,
    headers: &HeaderMap,
    endpoint: Endpoint,
    action: Action,
) -> GResult<()> {
    authorize_service(state, headers, endpoint, action)
        .map(|_| ())
        .map_err(|resp| {
            // Keep the internal API's own refusal bodies (`invalid_service_token`, …).
            let status = resp.status();
            let code = if status == StatusCode::UNAUTHORIZED {
                "invalid_service_token"
            } else if status == StatusCode::FORBIDDEN {
                "endpoint_not_allowed"
            } else {
                "forbidden"
            };
            GErr::new(status, code, "service not allowed")
        })
}

/// The caller's account, for the member routes: the account's own credential (a person's session
/// or the account API key), never a mailbox token or a connector acting for the member.
async fn member(state: &AppState, headers: &HeaderMap) -> GResult<(Principal, String)> {
    let principal = principal_for_token(state, bearer(headers)).await?;
    let account = principal_account(&principal)
        .map(str::to_string)
        .ok_or_else(|| {
            GErr::forbidden(
                "credential_not_accepted",
                "an account credential is required",
            )
        })?;
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&principal),
        Action::MeetingNotes,
        &Resource::Account(&account),
        &facts,
    )
    .map_err(|deny| {
        let resp = deny.into_response();
        GErr::new(
            resp.status(),
            "credential_not_accepted",
            "this credential cannot open meeting notes",
        )
    })?;
    Ok((principal, account))
}

async fn flag_on(state: &AppState, account: &str) -> bool {
    crate::filtering::feature_on(state, Some(account), capabilities::MEETING_SUMMARIES).await
}

fn audit(
    state: &AppState,
    kind: ActorKind,
    account: Option<String>,
    action: &str,
    meeting: &str,
    details: Value,
    now: u64,
) -> crate::store::AuditRow {
    state.auditor.row(
        kind,
        account,
        action,
        Some(format!("meeting:{meeting}")),
        details,
        None,
        None,
        now,
    )
}

fn respond<T: serde::Serialize>(r: GResult<T>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

// ---- internal: calls service -------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenReq {
    /// The host's mailbox address, as the calls service authenticated it.
    host: String,
    /// An opaque reference to the call room (the calls service hashes its room id).
    #[serde(default)]
    room_ref: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    calendar_event_id: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
    /// Only answer whether the host may request notes; create nothing.
    #[serde(default)]
    check_only: bool,
}

/// `POST /internal/v1/meetings` — may this host request notes, and if so open a meeting record.
async fn open(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<OpenReq>>,
) -> Response {
    respond(open_inner(&state, &headers, body).await)
}

async fn open_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: Option<Json<OpenReq>>,
) -> GResult<Value> {
    service(
        state,
        headers,
        Endpoint::MeetingsCalls,
        Action::MeetingsCalls,
    )?;
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "malformed meeting request"))?;
    let host = state
        .store
        .get(req.host.trim().to_string())
        .await?
        .filter(|h| h.account_id.is_some());
    let Some(host) = host else {
        return Ok(json!({ "allowed": false, "reason": "unknown_host" }));
    };
    let account = host.account_id.clone().unwrap_or_default();
    if !flag_on(state, &account).await {
        return Ok(json!({ "allowed": false, "reason": "feature_disabled" }));
    }
    let retention_days = retention_secs() / 86_400;
    if req.check_only {
        return Ok(json!({ "allowed": true, "retention_days": retention_days }));
    }
    let kind = req.kind.as_deref().unwrap_or("room");
    if !matches!(kind, "direct" | "room" | "group" | "scheduled") {
        return Err(GErr::bad("bad_request", "unknown meeting kind"));
    }
    let room_ref = req.room_ref.unwrap_or_default();
    if room_ref.is_empty()
        || room_ref.len() > 64
        || !room_ref.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(GErr::bad(
            "bad_request",
            "room_ref must be 1-64 hex characters",
        ));
    }
    for v in [&req.group_id, &req.calendar_event_id, &req.occurrence_id]
        .into_iter()
        .flatten()
    {
        if v.len() > 120 {
            return Err(GErr::bad("bad_request", "identifier too long"));
        }
    }
    crate::groups::rate_limit(&format!("meetings-open:{account}"), 30, 3_600)?;
    let now = now_unix();
    let id = format!("mtg_{}", crate::rand_hex(13));
    let expires_at = now + retention_secs();
    let row = audit(
        state,
        ActorKind::System,
        Some(account.clone()),
        "meeting.notes_started",
        &id,
        json!({ "kind": kind }),
        now,
    );
    let (mid, host_account, host_address) = (id.clone(), account.clone(), host.address.clone());
    let kind = kind.to_string();
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "INSERT INTO meetings (id, room_ref, kind, host_account, host_address, group_id,
                    calendar_event_id, occurrence_id, state, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?10)",
                params![
                    mid,
                    room_ref,
                    kind,
                    host_account,
                    host_address,
                    req.group_id,
                    req.calendar_event_id,
                    req.occurrence_id,
                    now,
                    expires_at
                ],
            )?;
            crate::store::audit::append_in(tx, &row)?;
            Ok(())
        })
        .await?;
    Ok(json!({
        "allowed": true,
        "meeting_id": id,
        "retention_days": retention_days,
        "expires_at": expires_at,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotesKey {
    key_id: String,
    public_key: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttendeeReq {
    /// The member's mailbox address, as the calls service authenticated it.
    address: String,
    /// `yes`, `no` or `withdrawn`.
    consent: String,
    /// The consenting device's notes key, registered for the member's account.
    #[serde(default)]
    notes_key: Option<NotesKey>,
}

/// `POST /internal/v1/meetings/{id}/attendees` — record one member's consent answer.
async fn attendee(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<AttendeeReq>>,
) -> Response {
    respond(attendee_inner(&state, &headers, id, body).await)
}

async fn attendee_inner(
    state: &AppState,
    headers: &HeaderMap,
    id: String,
    body: Option<Json<AttendeeReq>>,
) -> GResult<Value> {
    service(
        state,
        headers,
        Endpoint::MeetingsCalls,
        Action::MeetingsCalls,
    )?;
    if !valid_meeting_id(&id) {
        return Err(not_found());
    }
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "malformed attendee record"))?;
    if !matches!(req.consent.as_str(), "yes" | "no" | "withdrawn") {
        return Err(GErr::bad(
            "bad_request",
            "consent must be yes, no or withdrawn",
        ));
    }
    if let Some(k) = &req.notes_key {
        if req.consent != "yes" {
            return Err(GErr::bad(
                "bad_request",
                "a notes key comes only with a yes",
            ));
        }
        if !valid_key_id(&k.key_id) {
            return Err(GErr::bad("bad_request", "bad notes key id"));
        }
        valid_p256("public_key", &k.public_key)?;
    }
    let who = state
        .store
        .get(req.address.trim().to_string())
        .await?
        .and_then(|i| i.account_id.map(|a| (a, i.address)));
    let Some((account, address)) = who else {
        return Err(GErr::bad("unknown_member", "no such member mailbox"));
    };
    let now = now_unix();
    let row = audit(
        state,
        ActorKind::System,
        Some(account.clone()),
        "meeting.consent",
        &id,
        json!({ "consent": req.consent, "notes_key": req.notes_key.is_some() }),
        now,
    );
    state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let meeting: Option<String> = tx
                .query_row("SELECT state FROM meetings WHERE id = ?1", params![id], |r| r.get(0))
                .optional()?;
            match meeting.as_deref() {
                None => return Err(not_found()),
                Some("active") => {}
                Some(_) => return Err(GErr::conflict("meeting_closed", "notes for this meeting have ended")),
            }
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM meeting_attendees WHERE meeting_id = ?1 AND account_id != ?2",
                params![id, account],
                |r| r.get(0),
            )?;
            if count as usize >= MAX_ATTENDEES {
                return Err(GErr::conflict("attendee_limit", "too many attendees"));
            }
            tx.execute(
                "INSERT INTO meeting_attendees (meeting_id, account_id, address) VALUES (?1, ?2, ?3)
                 ON CONFLICT (meeting_id, account_id) DO NOTHING",
                params![id, account, address],
            )?;
            match req.consent.as_str() {
                "yes" => tx.execute(
                    "UPDATE meeting_attendees SET consented_at = COALESCE(consented_at, ?3),
                         withdrawn_at = NULL WHERE meeting_id = ?1 AND account_id = ?2",
                    params![id, account, now],
                )?,
                "no" => tx.execute(
                    "UPDATE meeting_attendees SET declined_at = ?3 WHERE meeting_id = ?1 AND account_id = ?2",
                    params![id, account, now],
                )?,
                _ => tx.execute(
                    "UPDATE meeting_attendees SET withdrawn_at = ?3 WHERE meeting_id = ?1 AND account_id = ?2",
                    params![id, account, now],
                )?,
            };
            if let Some(k) = req.notes_key {
                upsert_key(tx, &account, &k, now)?;
            }
            crate::store::audit::append_in(tx, &row)?;
            Ok(json!({ "recorded": true }))
        })
        .await
}

fn upsert_key(tx: &rusqlite::Connection, account: &str, k: &NotesKey, now: u64) -> GResult<()> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT public_key FROM meeting_notes_keys WHERE account_id = ?1 AND key_id = ?2",
            params![account, k.key_id],
            |r| r.get(0),
        )
        .optional()?;
    match existing {
        Some(pk) if pk == k.public_key => {
            tx.execute(
                "UPDATE meeting_notes_keys SET last_used_at = ?3 WHERE account_id = ?1 AND key_id = ?2",
                params![account, k.key_id, now],
            )?;
        }
        // A key id is never rebound to a different key: a device makes a new id instead.
        Some(_) => {
            return Err(GErr::conflict(
                "key_exists",
                "this key id holds a different key",
            ))
        }
        None => {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM meeting_notes_keys WHERE account_id = ?1",
                params![account],
                |r| r.get(0),
            )?;
            if count as usize >= MAX_KEYS {
                // Make room by forgetting the key used longest ago.
                tx.execute(
                    "DELETE FROM meeting_notes_keys WHERE account_id = ?1 AND key_id = (
                        SELECT key_id FROM meeting_notes_keys WHERE account_id = ?1
                        ORDER BY COALESCE(last_used_at, created_at) ASC LIMIT 1)",
                    params![account],
                )?;
            }
            let label: Option<String> = k
                .label
                .as_ref()
                .map(|l| l.chars().filter(|c| !c.is_control()).take(60).collect());
            tx.execute(
                "INSERT INTO meeting_notes_keys (account_id, key_id, public_key, label, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![account, k.key_id, k.public_key, label, now],
            )?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EndReq {
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /internal/v1/meetings/{id}/end` — capture is over; the brief is being made.
async fn end(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<EndReq>>,
) -> Response {
    respond(end_inner(&state, &headers, id, body).await)
}

async fn end_inner(
    state: &AppState,
    headers: &HeaderMap,
    id: String,
    body: Option<Json<EndReq>>,
) -> GResult<Value> {
    service(
        state,
        headers,
        Endpoint::MeetingsCalls,
        Action::MeetingsCalls,
    )?;
    if !valid_meeting_id(&id) {
        return Err(not_found());
    }
    let reason: String = body
        .and_then(|Json(b)| b.reason)
        .unwrap_or_else(|| "stopped".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect();
    let now = now_unix();
    let row = audit(
        state,
        ActorKind::System,
        None,
        "meeting.notes_ended",
        &id,
        json!({ "reason": reason }),
        now,
    );
    state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let changed = tx.execute(
                "UPDATE meetings SET state = 'processing', ended_at = ?2 WHERE id = ?1 AND state = 'active'",
                params![id, now],
            )?;
            let exists: bool = tx
                .query_row("SELECT 1 FROM meetings WHERE id = ?1", params![id], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Err(not_found());
            }
            if changed > 0 {
                crate::store::audit::append_in(tx, &row)?;
            }
            Ok(json!({ "ended": changed > 0 }))
        })
        .await
}

// ---- internal: meeting-intel -------------------------------------------------------------------

/// `GET /internal/v1/meetings/{id}/recipients` — the consenting attendees and their notes keys.
async fn recipients(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    respond(recipients_inner(&state, &headers, id).await)
}

async fn recipients_inner(state: &AppState, headers: &HeaderMap, id: String) -> GResult<Value> {
    service(
        state,
        headers,
        Endpoint::MeetingsIntel,
        Action::MeetingsIntel,
    )?;
    if !valid_meeting_id(&id) {
        return Err(not_found());
    }
    state
        .store
        .group_read(move |c| -> GResult<Value> {
            let st: Option<String> = c
                .query_row(
                    "SELECT state FROM meetings WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            match st.as_deref() {
                None => return Err(not_found()),
                Some("active" | "processing") => {}
                Some(_) => {
                    return Err(GErr::conflict(
                        "meeting_closed",
                        "this meeting already has a result",
                    ))
                }
            }
            let mut stmt = c.prepare(
                "SELECT a.account_id, a.address, k.key_id, k.public_key
                   FROM meeting_attendees a
                   JOIN meeting_notes_keys k ON k.account_id = a.account_id
                  WHERE a.meeting_id = ?1 AND a.consented_at IS NOT NULL AND a.removed_at IS NULL
                  ORDER BY a.account_id, k.key_id",
            )?;
            let rows = stmt.query_map(params![id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut out: Vec<Value> = Vec::new();
            for row in rows {
                let (account, address, key_id, public_key) = row?;
                let key = json!({ "key_id": key_id, "public_key": public_key });
                match out.last_mut() {
                    Some(last) if last["account_id"] == json!(account) => {
                        last["keys"].as_array_mut().expect("keys array").push(key);
                    }
                    _ => out
                        .push(json!({ "account_id": account, "address": address, "keys": [key] })),
                }
            }
            Ok(json!({ "meeting_id": id, "recipients": out }))
        })
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Wrap {
    account_id: String,
    key_id: String,
    epk: String,
    iv: String,
    wrapped: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BriefReq {
    /// `ready` or `failed`.
    status: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    iv: Option<String>,
    #[serde(default)]
    ciphertext: Option<String>,
    #[serde(default)]
    wraps: Vec<Wrap>,
    /// For `failed`: a short machine reason (`no_speech`, `stt_unavailable`, …).
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /internal/v1/meetings/{id}/brief` — store the encrypted brief and tell the attendees.
async fn store_brief(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<BriefReq>>,
) -> Response {
    respond(brief_inner(&state, &headers, id, body).await)
}

async fn brief_inner(
    state: &AppState,
    headers: &HeaderMap,
    id: String,
    body: Option<Json<BriefReq>>,
) -> GResult<Value> {
    service(
        state,
        headers,
        Endpoint::MeetingsIntel,
        Action::MeetingsIntel,
    )?;
    if !valid_meeting_id(&id) {
        return Err(not_found());
    }
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "malformed brief"))?;
    let ready = match req.status.as_str() {
        "ready" => true,
        "failed" => false,
        _ => return Err(GErr::bad("bad_request", "status must be ready or failed")),
    };
    if ready {
        if req.alg.as_deref() != Some("A256GCM") {
            return Err(GErr::bad("bad_request", "alg must be A256GCM"));
        }
        b64_of("iv", req.iv.as_deref().unwrap_or(""), 12)?;
        let ct = req.ciphertext.as_deref().unwrap_or("");
        if ct.is_empty() || ct.len() > MAX_CIPHERTEXT_CHARS {
            return Err(GErr::bad("bad_request", "ciphertext missing or too large"));
        }
        b64_of("ciphertext", ct, 0)?;
        if req.wraps.is_empty() || req.wraps.len() > MAX_ATTENDEES * MAX_KEYS {
            return Err(GErr::bad("bad_request", "wraps missing or too many"));
        }
        for w in &req.wraps {
            if !valid_key_id(&w.key_id) {
                return Err(GErr::bad("bad_request", "bad key id"));
            }
            valid_p256("epk", &w.epk)?;
            b64_of("iv", &w.iv, 12)?;
            b64_of("wrapped", &w.wrapped, 48)?;
        }
    } else if !req.wraps.is_empty() || req.ciphertext.is_some() {
        return Err(GErr::bad(
            "bad_request",
            "a failed result carries no content",
        ));
    }
    let reason: Option<String> = req.reason.as_ref().map(|r| {
        r.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(32)
            .collect()
    });
    let now = now_unix();
    let bytes = req.ciphertext.as_ref().map(|c| c.len()).unwrap_or(0);
    let row = audit(
        state,
        ActorKind::System,
        None,
        if ready {
            "meeting.brief_stored"
        } else {
            "meeting.brief_failed"
        },
        &id,
        json!({ "wraps": req.wraps.len(), "bytes": bytes, "reason": reason }),
        now,
    );
    let mid = id.clone();
    let woke = state
        .store
        .group_tx(move |tx| -> GResult<Vec<String>> {
            let st: Option<String> = tx
                .query_row("SELECT state FROM meetings WHERE id = ?1", params![mid], |r| r.get(0))
                .optional()?;
            match st.as_deref() {
                None => return Err(not_found()),
                Some("active" | "processing") => {}
                Some(_) => return Err(GErr::conflict("meeting_closed", "this meeting already has a result")),
            }
            // Every wrap must name a consenting attendee and one of that attendee's registered keys:
            // meeting-intel cannot widen the audience.
            for w in &req.wraps {
                let ok: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM meeting_attendees a JOIN meeting_notes_keys k
                            ON k.account_id = a.account_id AND k.key_id = ?3
                          WHERE a.meeting_id = ?1 AND a.account_id = ?2
                            AND a.consented_at IS NOT NULL AND a.removed_at IS NULL",
                        params![mid, w.account_id, w.key_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if ok.is_none() {
                    return Err(GErr::forbidden("not_a_recipient", "a wrap names someone outside the audience"));
                }
            }
            if ready {
                tx.execute(
                    "UPDATE meetings SET state = 'ready', ready_at = ?2, ended_at = COALESCE(ended_at, ?2),
                        alg = ?3, iv = ?4, ciphertext = ?5 WHERE id = ?1",
                    params![mid, now, req.alg, req.iv, req.ciphertext],
                )?;
                for w in &req.wraps {
                    tx.execute(
                        "INSERT OR REPLACE INTO meeting_brief_keys (meeting_id, account_id, key_id, epk, iv, wrapped)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![mid, w.account_id, w.key_id, w.epk, w.iv, w.wrapped],
                    )?;
                }
            } else {
                tx.execute(
                    "UPDATE meetings SET state = 'failed', ended_at = COALESCE(ended_at, ?2), failure = ?3
                      WHERE id = ?1",
                    params![mid, now, reason],
                )?;
            }
            crate::store::audit::append_in(tx, &row)?;
            // Tell every consenting attendee, on the mailbox they took part as.
            let mut stmt = tx.prepare(
                "SELECT account_id, address FROM meeting_attendees
                  WHERE meeting_id = ?1 AND consented_at IS NOT NULL AND removed_at IS NULL",
            )?;
            let targets: Vec<(String, Option<String>)> = stmt
                .query_map(params![mid], |r| Ok((r.get::<_, String>(1)?, Some(r.get::<_, String>(0)?))))?
                .collect::<Result<_, _>>()?;
            let data = json!({ "meeting_id": mid, "state": if ready { "ready" } else { "failed" } });
            crate::groups::emit(tx, &targets, "meeting.brief", &data, now)
        })
        .await?;
    for account in &woke {
        state.event_hub.wake(account);
    }
    Ok(json!({ "stored": true, "notified": woke.len() }))
}

// ---- member routes -----------------------------------------------------------------------------

/// `GET /v1/meetings` — the meetings whose notes the caller agreed to, newest first.
async fn list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    respond(list_inner(&state, &headers).await)
}

async fn list_inner(state: &AppState, headers: &HeaderMap) -> GResult<Value> {
    let (_, account) = member(state, headers).await?;
    let now = now_unix();
    state
        .store
        .group_read(move |c| -> GResult<Value> {
            let mut stmt = c.prepare(
                "SELECT m.id, m.kind, m.state, m.created_at, m.ended_at, m.ready_at, m.expires_at,
                        m.host_address, a.address, m.failure
                   FROM meetings m JOIN meeting_attendees a ON a.meeting_id = m.id
                  WHERE a.account_id = ?1 AND a.consented_at IS NOT NULL AND a.removed_at IS NULL
                    AND m.expires_at > ?2
                  ORDER BY m.created_at DESC LIMIT 200",
            )?;
            let rows = stmt
                .query_map(params![account, now], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "kind": r.get::<_, String>(1)?,
                        "state": r.get::<_, String>(2)?,
                        "created_at": r.get::<_, i64>(3)?,
                        "ended_at": r.get::<_, Option<i64>>(4)?,
                        "ready_at": r.get::<_, Option<i64>>(5)?,
                        "expires_at": r.get::<_, i64>(6)?,
                        "host": r.get::<_, String>(7)?,
                        "as": r.get::<_, String>(8)?,
                        "failure": r.get::<_, Option<String>>(9)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "meetings": rows }))
        })
        .await
}

/// `GET /v1/meetings/{id}` — status, and when ready the ciphertext with the caller's own wraps.
async fn fetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    respond(fetch_inner(&state, &headers, id).await)
}

async fn fetch_inner(state: &AppState, headers: &HeaderMap, id: String) -> GResult<Value> {
    let (_, account) = member(state, headers).await?;
    if !valid_meeting_id(&id) {
        return Err(not_found());
    }
    let now = now_unix();
    state
        .store
        .group_read(move |c| -> GResult<Value> {
            type Row = (
                String,
                String,
                i64,
                Option<i64>,
                Option<i64>,
                i64,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                String,
            );
            let row: Option<Row> = c
                .query_row(
                    "SELECT m.kind, m.state, m.created_at, m.ended_at, m.ready_at, m.expires_at,
                            m.alg, m.iv, m.ciphertext, m.failure, m.host_address
                       FROM meetings m JOIN meeting_attendees a ON a.meeting_id = m.id
                      WHERE m.id = ?1 AND a.account_id = ?2 AND a.consented_at IS NOT NULL
                        AND a.removed_at IS NULL AND m.expires_at > ?3",
                    params![id, account, now],
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
                            r.get(10)?,
                        ))
                    },
                )
                .optional()?;
            // Absent, expired, removed, declined or never attended all look the same.
            let Some((kind, st, created, ended, ready_at, expires, alg, iv, ct, failure, host)) =
                row
            else {
                return Err(not_found());
            };
            let mut out = json!({
                "id": id, "kind": kind, "state": st, "created_at": created, "ended_at": ended,
                "ready_at": ready_at, "expires_at": expires, "host": host, "failure": failure,
            });
            if st == "ready" {
                let mut stmt = c.prepare(
                    "SELECT key_id, epk, iv, wrapped FROM meeting_brief_keys
                      WHERE meeting_id = ?1 AND account_id = ?2 ORDER BY key_id",
                )?;
                let keys = stmt
                    .query_map(params![id, account], |r| {
                        Ok(json!({
                            "key_id": r.get::<_, String>(0)?,
                            "epk": r.get::<_, String>(1)?,
                            "iv": r.get::<_, String>(2)?,
                            "wrapped": r.get::<_, String>(3)?,
                        }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                out["alg"] = json!(alg);
                out["iv"] = json!(iv);
                out["ciphertext"] = json!(ct);
                out["keys"] = json!(keys);
            }
            Ok(out)
        })
        .await
}

/// `DELETE /v1/meetings/{id}` — remove the caller's copy. The last copy takes the brief with it.
async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    respond(remove_inner(&state, &headers, id).await)
}

async fn remove_inner(state: &AppState, headers: &HeaderMap, id: String) -> GResult<Value> {
    let (principal, account) = member(state, headers).await?;
    if !valid_meeting_id(&id) {
        return Err(not_found());
    }
    let now = now_unix();
    let row = audit(
        state,
        ActorKind::of(&principal),
        Some(account.clone()),
        "meeting.brief_removed",
        &id,
        json!({}),
        now,
    );
    state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let changed = tx.execute(
                "UPDATE meeting_attendees SET removed_at = ?3
                  WHERE meeting_id = ?1 AND account_id = ?2 AND consented_at IS NOT NULL AND removed_at IS NULL",
                params![id, account, now],
            )?;
            if changed == 0 {
                return Err(not_found());
            }
            tx.execute(
                "DELETE FROM meeting_brief_keys WHERE meeting_id = ?1 AND account_id = ?2",
                params![id, account],
            )?;
            let left: i64 = tx.query_row(
                "SELECT COUNT(*) FROM meeting_attendees
                  WHERE meeting_id = ?1 AND consented_at IS NOT NULL AND removed_at IS NULL",
                params![id],
                |r| r.get(0),
            )?;
            let st: String = tx.query_row("SELECT state FROM meetings WHERE id = ?1", params![id], |r| r.get(0))?;
            let gone = left == 0 && matches!(st.as_str(), "ready" | "failed");
            if gone {
                delete_meeting(tx, &id)?;
            }
            crate::store::audit::append_in(tx, &row)?;
            Ok(json!({ "removed": true, "brief_deleted": gone }))
        })
        .await
}

fn delete_meeting(tx: &rusqlite::Connection, id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM meeting_brief_keys WHERE meeting_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM meeting_attendees WHERE meeting_id = ?1",
        params![id],
    )?;
    tx.execute("DELETE FROM meetings WHERE id = ?1", params![id])?;
    Ok(())
}

/// `GET /v1/meetings/keys` — the caller's registered notes keys (public halves only).
async fn list_keys(State(state): State<AppState>, headers: HeaderMap) -> Response {
    respond(list_keys_inner(&state, &headers).await)
}

async fn list_keys_inner(state: &AppState, headers: &HeaderMap) -> GResult<Value> {
    let (_, account) = member(state, headers).await?;
    state
        .store
        .group_read(move |c| -> GResult<Value> {
            let mut stmt = c.prepare(
                "SELECT key_id, public_key, label, created_at, last_used_at FROM meeting_notes_keys
                  WHERE account_id = ?1 ORDER BY created_at",
            )?;
            let keys = stmt
                .query_map(params![account], |r| {
                    Ok(json!({
                        "key_id": r.get::<_, String>(0)?,
                        "public_key": r.get::<_, String>(1)?,
                        "label": r.get::<_, Option<String>>(2)?,
                        "created_at": r.get::<_, i64>(3)?,
                        "last_used_at": r.get::<_, Option<i64>>(4)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "keys": keys }))
        })
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutKeyReq {
    public_key: String,
    #[serde(default)]
    label: Option<String>,
}

/// `PUT /v1/meetings/keys/{key_id}` — register this device's notes key (idempotent).
async fn put_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    body: Option<Json<PutKeyReq>>,
) -> Response {
    respond(put_key_inner(&state, &headers, key_id, body).await)
}

async fn put_key_inner(
    state: &AppState,
    headers: &HeaderMap,
    key_id: String,
    body: Option<Json<PutKeyReq>>,
) -> GResult<Value> {
    let (principal, account) = member(state, headers).await?;
    if !flag_on(state, &account).await {
        return Err(feature_off());
    }
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "malformed key"))?;
    if !valid_key_id(&key_id) {
        return Err(GErr::bad("bad_request", "bad key id"));
    }
    valid_p256("public_key", &req.public_key)?;
    crate::groups::rate_limit(&format!("meetings-key:{account}"), 30, 3_600)?;
    let now = now_unix();
    let row = state.auditor.row(
        ActorKind::of(&principal),
        Some(account.clone()),
        "meeting.notes_key_registered",
        Some(format!("notes_key:{key_id}")),
        json!({}),
        None,
        None,
        now,
    );
    let k = NotesKey {
        key_id,
        public_key: req.public_key,
        label: req.label,
    };
    state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            upsert_key(tx, &account, &k, now)?;
            crate::store::audit::append_in(tx, &row)?;
            Ok(json!({ "key_id": k.key_id, "registered": true }))
        })
        .await
}

/// `DELETE /v1/meetings/keys/{key_id}` — forget a device's key (future briefs skip it).
async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
) -> Response {
    respond(delete_key_inner(&state, &headers, key_id).await)
}

async fn delete_key_inner(state: &AppState, headers: &HeaderMap, key_id: String) -> GResult<Value> {
    let (principal, account) = member(state, headers).await?;
    let now = now_unix();
    let row = state.auditor.row(
        ActorKind::of(&principal),
        Some(account.clone()),
        "meeting.notes_key_deleted",
        Some(format!("notes_key:{key_id}")),
        json!({}),
        None,
        None,
        now,
    );
    state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let n = tx.execute(
                "DELETE FROM meeting_notes_keys WHERE account_id = ?1 AND key_id = ?2",
                params![account, key_id],
            )?;
            if n == 0 {
                return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no such key"));
            }
            tx.execute(
                "DELETE FROM meeting_brief_keys WHERE account_id = ?1 AND key_id = ?2",
                params![account, key_id],
            )?;
            crate::store::audit::append_in(tx, &row)?;
            Ok(json!({ "deleted": true }))
        })
        .await
}

// ---- retention ---------------------------------------------------------------------------------

/// Delete expired meetings and close meetings that never produced a result. Run by the reaper.
pub(crate) async fn purge(store: &crate::store::Store, now: u64) -> Result<(usize, usize), GErr> {
    store
        .group_tx(move |tx| -> GResult<(usize, usize)> {
            let stale = tx.execute(
                "UPDATE meetings SET state = 'failed', failure = 'timed_out', ended_at = COALESCE(ended_at, ?1)
                  WHERE state IN ('active','processing') AND created_at < ?2",
                params![now, now.saturating_sub(PENDING_SECS)],
            )?;
            let ids: Vec<String> = {
                let mut stmt = tx.prepare("SELECT id FROM meetings WHERE expires_at <= ?1")?;
                let ids = stmt
                    .query_map(params![now], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                ids
            };
            for id in &ids {
                delete_meeting(tx, id)?;
            }
            Ok((ids.len(), stale))
        })
        .await
}
