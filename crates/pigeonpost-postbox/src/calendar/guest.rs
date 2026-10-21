//! Routes for contact-address guests: people without a Pigeonpost account (SDS §14B.5 public
//! endpoints, §14B.8; `docs/calendar.md` §6).
//!
//! Separate credentials, each for one purpose:
//! * the **invitation link** token (`invite_manage`, ≤ 30 days) — preview, answer, export, and ask
//!   to join (always waiting for a host);
//! * a **verification code** (single use, 10 minutes, stored as a keyed MAC) sent only to the
//!   address the organizer entered;
//! * a **guest session** (≤ 24 h) bound to the browser's P-256 key and the attendee's current
//!   grant generation — the only thing that can get direct (lobby-policy) admission;
//! * a **join ticket** (single use, ≤ 2 min) the calls service redeems;
//! * an **unsubscribe** token that can only record a suppression.
//!
//! Every refusal is generic (`guest_token_invalid`): a guessed or stale value learns nothing.

use super::events::{mint_invite_token, record_rsvp, RsvpIn};
use super::*;
use crate::now_unix;
use axum::extract::{Extension, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Transaction;
use serde::Deserialize;

const SESSION_SECS: i64 = 24 * 3600;
const CHALLENGE_SECS: i64 = 600;
const TICKET_SECS: i64 = 120;
const PROOF_SKEW: i64 = 120;

fn invalid() -> GErr {
    GErr::new(
        StatusCode::FORBIDDEN,
        "guest_token_invalid",
        "this link is not valid any more; ask the organizer for a new one",
    )
}

fn ip_of(ip: &Option<Extension<crate::ClientIp>>) -> String {
    ip.as_ref()
        .map(|Extension(i)| i.as_str().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn json_body<T: serde::de::DeserializeOwned>(
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> GResult<T> {
    let Json(v) = body.map_err(|e| GErr::bad("bad_request", e.body_text()))?;
    serde_json::from_value(v).map_err(|e| GErr::bad("bad_request", e.to_string()))
}

/// A live invitation behind a link token: `(token id, attendee, event)`.
fn by_token(
    c: &Connection,
    vault: &crate::vault::Vault,
    token: &str,
    now: i64,
) -> GResult<(String, Att, Ev)> {
    if token.len() != 43 || unb64url(token).is_none() {
        return Err(invalid());
    }
    let row: Option<(String, String, i64)> = c
        .query_row(
            "SELECT id, attendee_id, attendee_grant_version FROM event_attendee_tokens
              WHERE token_hash = ?1 AND purpose = 'invite_manage' AND revoked_at IS NULL AND expires_at > ?2",
            params![secret_hash(token), now],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (tid, aid, grant) = row.ok_or_else(invalid)?;
    let att = attendee(c, &aid)?.ok_or_else(invalid)?;
    if att.grant_version != grant
        || att.removed_at.is_some()
        || att.redacted_at.is_some()
        || att.kind != "external"
    {
        return Err(invalid());
    }
    let ev = load_ev(c, vault, &att.event_id)?.ok_or_else(invalid)?;
    Ok((tid, att, ev))
}

/// A live guest session: `(session id, attendee, event)`.
fn by_session(
    c: &Connection,
    vault: &crate::vault::Vault,
    session: &str,
    now: i64,
) -> GResult<(String, Att, Ev, Vec<u8>)> {
    if session.len() != 43 || unb64url(session).is_none() {
        return Err(invalid());
    }
    let row: Option<(String, String, i64, Vec<u8>)> = c
        .query_row(
            "SELECT id, attendee_id, attendee_grant_version, device_key FROM event_guest_sessions
              WHERE token_hash = ?1 AND revoked_at IS NULL AND expires_at > ?2",
            params![secret_hash(session), now],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (sid, aid, grant, key) = row.ok_or_else(invalid)?;
    let att = attendee(c, &aid)?.ok_or_else(invalid)?;
    if att.grant_version != grant || att.removed_at.is_some() || att.redacted_at.is_some() {
        return Err(invalid());
    }
    let ev = load_ev(c, vault, &att.event_id)?.ok_or_else(invalid)?;
    Ok((sid, att, ev, key))
}

/// Verify a device proof over `pigeonpost-guest-proof-v1\n<purpose>\n<binding>\n<ts>`.
pub(crate) fn verify_proof(
    public_key: &[u8],
    purpose: &str,
    binding: &str,
    ts: i64,
    proof: &str,
    now: i64,
) -> bool {
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::{Signature, VerifyingKey};
    if (ts - now).abs() > PROOF_SKEW {
        return false;
    }
    let Some(sig) = unb64url(proof) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_sec1_bytes(public_key) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&sig) else {
        return false;
    };
    let sig = sig.normalize_s().unwrap_or(sig);
    let msg = format!("pigeonpost-guest-proof-v1\n{purpose}\n{binding}\n{ts}");
    vk.verify(msg.as_bytes(), &sig).is_ok()
}

fn external_ready(state: &AppState) -> bool {
    state.calendar.notifier.configured()
}

async fn external_on_for(state: &AppState, ev_account: Option<String>) -> bool {
    external_ready(state)
        && feature_on(state, ev_account.as_deref(), capabilities::CALENDAR_EMAIL).await
}

/// The calendar feature must be on for the organizer's account.
async fn calendar_on_for(state: &AppState, account: Option<String>) -> bool {
    feature_on(state, account.as_deref(), capabilities::CALENDAR).await
}

// ---- preview, export, answer -------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct TokenIn {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    guest_session: Option<String>,
}

/// Resolve either credential to `(attendee, event, proof level)`.
fn either(
    c: &Connection,
    vault: &crate::vault::Vault,
    t: &TokenIn,
    now: i64,
) -> GResult<(Att, Ev, &'static str)> {
    match (&t.token, &t.guest_session) {
        (Some(tok), _) => by_token(c, vault, tok, now).map(|(_, a, e)| (a, e, "link")),
        (None, Some(s)) => by_session(c, vault, s, now).map(|(_, a, e, _)| (a, e, "verified")),
        _ => Err(invalid()),
    }
}

/// `POST /v1/calendar/guest/preview`
pub(crate) async fn preview(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-guest:{}", ip_of(&ip)), 120, 600)?;
        let t: TokenIn = json_body(body)?;
        let vault = state.vault.clone();
        let inbox = state.calendar.inbox_url.clone();
        let now = now_unix() as i64;
        let (v, account) = state
            .store
            .group_read(move |c: &Connection| -> GResult<(Value, Option<String>)> {
                let (att, ev, proof) = either(c, &vault, &t, now)?;
                let mut next = Vec::new();
                if let Ok(occs) = occurrences_in(c, &vault, &ev, now - 6 * 3600, now + time::MAX_WINDOW_SECS, time::MAX_OCCURRENCES) {
                    for o in occs {
                        if standing(c, &att, Some(&o.id))?.invited && o.ends_at + JOIN_AFTER_SECS > now {
                            let (opens, closes) = window(&ev, &o, extended_until(c, &o.id)?);
                            let mut j = super::events::occ_json(&o);
                            j["opens_at"] = json!(opens);
                            j["closes_at"] = json!(closes);
                            j["my_rsvp"] = json!(standing(c, &att, Some(&o.id))?.rsvp);
                            next.push(j);
                            if next.len() >= 10 {
                                break;
                            }
                        }
                    }
                }
                let guests = if ev.meeting.as_ref().is_some_and(|m| m.guests_see_list) {
                    let mut list = vec![json!({ "name": super::ics::organizer_name(c, &ev)?, "rsvp": "accepted", "organizer": true })];
                    for a in live_attendees(c, &ev.id)? {
                        if a.suppressed || a.base != "invited" {
                            continue;
                        }
                        let name = match &a.address {
                            Some(addr) => handle_of(c, addr)?.unwrap_or_else(|| "Pigeonpost member".into()),
                            None => a.display_name.clone().unwrap_or_else(|| "Guest".into()),
                        };
                        list.push(json!({ "name": name, "rsvp": a.rsvp }));
                    }
                    Value::Array(list)
                } else {
                    Value::Null
                };
                let mut event = super::events::timing_json(&ev);
                let first = ev.series().first().map(|i| effective(&ev, &i, None));
                let meeting = ev.meeting_code.as_ref().map(|code| {
                    let (opens, closes) = first.as_ref().map(|o| window(&ev, o, None)).unwrap_or((0, 0));
                    json!({ "code": code, "url": format!("{inbox}/m/{code}"), "opens_at": opens, "closes_at": closes })
                });
                if let Value::Object(m) = &mut event {
                    m.insert("title".into(), json!(ev.title));
                    m.insert("location".into(), json!(ev.location));
                    m.insert("description".into(), json!(ev.description));
                    m.insert("organizer_name".into(), json!(super::ics::organizer_name(c, &ev)?));
                    m.insert("recurring".into(), json!(ev.recurring()));
                    m.insert("status".into(), json!(ev.status));
                    m.insert("meeting".into(), json!(meeting));
                    m.insert("rrule".into(), json!(ev.rrule));
                }
                Ok((
                    json!({
                        "event": event, "occurrences": next,
                        "attendee": { "name": att.display_name.clone().unwrap_or_else(|| "Guest".into()),
                                      "rsvp": att.rsvp, "proof": proof },
                        "guests": guests,
                    }),
                    ev.created_by_account.clone(),
                ))
            })
            .await?;
        if !calendar_on_for(&state, account.clone()).await {
            return Err(invalid());
        }
        let mut v = v;
        v["verification_available"] = json!(external_on_for(&state, account).await);
        Ok((200, v, Effects::default()))
    }
    .await;
    let mut resp = finish(&state, r);
    no_store(&mut resp);
    resp
}

fn no_store(resp: &mut Response) {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().expect("static"));
    h.insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().expect("static"),
    );
}

/// `POST /v1/calendar/guest/ics`
pub(crate) async fn ics(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-guest:{}", ip_of(&ip)), 120, 600)?;
        let t: TokenIn = json_body(body)?;
        let vault = state.vault.clone();
        let inbox = state.calendar.inbox_url.clone();
        let now = now_unix() as i64;
        state
            .store
            .group_read(move |c: &Connection| -> GResult<String> {
                let (att, ev, _) = either(c, &vault, &t, now)?;
                super::ics::export(c, &vault, &ev, Some(&att), &inbox)
            })
            .await
    }
    .await;
    match r {
        Ok(text) => (
            [
                (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"invite.ics\"",
                ),
            ],
            text,
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct GuestRsvpIn {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    guest_session: Option<String>,
    status: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
    #[serde(default)]
    expected_revision: Option<i64>,
}

/// `POST /v1/calendar/guest/rsvp`
pub(crate) async fn rsvp(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-guest:{}", ip_of(&ip)), 120, 600)?;
        let input: GuestRsvpIn = json_body(body)?;
        let vault = state.vault.clone();
        let now = now_unix();
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    let t = TokenIn {
                        token: input.token.clone(),
                        guest_session: input.guest_session.clone(),
                    };
                    let (att, ev, proof) = either(tx, &vault, &t, now as i64)?;
                    let r = RsvpIn {
                        status: input.status.clone(),
                        note: input.note.clone(),
                        scope: input.scope.clone(),
                        occurrence_id: input.occurrence_id.clone(),
                        expected_revision: input.expected_revision,
                    };
                    let mut fx = Effects::default();
                    let mut v = record_rsvp(tx, &vault, &ev, &att, &r, proof, &mut fx, now)?;
                    if let Value::Object(m) = &mut v {
                        m.remove("attendee_id");
                    }
                    Ok((200, v, fx))
                },
            )
            .await
    }
    .await;
    finish(&state, r)
}

// ---- verification ------------------------------------------------------------------------------

fn code_mac(vault: &crate::vault::Vault, challenge: &str, code: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let key = vault.subkey("calendar-code-v1");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("any key length");
    mac.update(challenge.as_bytes());
    mac.update(b"\0");
    mac.update(code.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Create a challenge for `att` and queue its message. Returns the challenge id.
fn new_challenge(
    tx: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    att: &Att,
    invitation: &str,
    now: i64,
) -> GResult<String> {
    let recent: i64 = tx.query_row(
        "SELECT COUNT(*) FROM event_guest_challenges WHERE attendee_id = ?1 AND created_at > ?2",
        params![att.id, now - 3600],
        |r| r.get(0),
    )?;
    if recent >= 3 {
        let mut e = GErr::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many codes; wait a while",
        );
        e.retry_after = Some(900);
        return Err(e);
    }
    let mut n = [0u8; 4];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut n);
    let code = format!("{:06}", u32::from_le_bytes(n) % 1_000_000);
    let cid = new_id("chl");
    tx.execute(
        "INSERT INTO event_guest_challenges (id, attendee_id, invitation_id, code_mac, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![cid, att.id, invitation, code_mac(vault, &cid, &code), now, now + CHALLENGE_SECS],
    )?;
    let dlv = new_id("dlv");
    let sealed = seal_v(vault, &format!("{dlv}:code"), &code)?;
    tx.execute(
        "INSERT INTO calendar_delivery_outbox (id, event_id, recipient_id, workflow, event_revision,
             recipient_revision, reminder_id, dedupe_key, due_at, state, created_at, account_id, snapshot_sealed)
         VALUES (?1, ?2, ?3, 'verify', ?4, ?5, ?6, ?7, ?8, 'queued', ?8, ?9, ?10)",
        params![dlv, ev.id, att.id, ev.revision, att.revision, cid, format!("verify:{cid}"), now,
                ev.created_by_account, sealed],
    )?;
    Ok(cid)
}

/// `POST /v1/calendar/guest/verify/start`
pub(crate) async fn verify_start(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-verify:{}", ip_of(&ip)), 10, 3600)?;
        let t: TokenIn = json_body(body)?;
        let token = t.token.ok_or_else(invalid)?;
        let vault = state.vault.clone();
        let now = now_unix() as i64;
        let account = {
            let vault = vault.clone();
            let token = token.clone();
            state
                .store
                .group_read(move |c: &Connection| -> GResult<Option<String>> {
                    Ok(by_token(c, &vault, &token, now)?.2.created_by_account)
                })
                .await?
        };
        if !external_on_for(&state, account).await {
            return Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "external_unavailable",
                "confirming your address is not available yet; you can still ask to join",
            ));
        }
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    let (tid, att, ev) = by_token(tx, &vault, &token, now)?;
                    if ev.status != "confirmed" {
                        return Err(invalid());
                    }
                    let cid = new_challenge(tx, &vault, &ev, &att, &tid, now)?;
                    let fx = Effects {
                        nudge: true,
                        ..Effects::default()
                    };
                    Ok((
                        200,
                        json!({ "challenge_id": cid, "expires_at": now + CHALLENGE_SECS }),
                        fx,
                    ))
                },
            )
            .await
    }
    .await;
    finish(&state, r)
}

#[derive(Deserialize, Default)]
pub(crate) struct CompleteIn {
    challenge_id: String,
    code: String,
    device_public_key: String,
    device_proof: String,
    ts: i64,
}

/// `POST /v1/calendar/guest/verify/complete`
#[allow(clippy::type_complexity)]
pub(crate) async fn verify_complete(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-verify-try:{}", ip_of(&ip)), 30, 3600)?;
        let input: CompleteIn = json_body(body)?;
        let key = unb64url(&input.device_public_key).filter(|k| k.len() == 65 && k[0] == 4).ok_or_else(invalid)?;
        let now = now_unix() as i64;
        if !verify_proof(&key, "verify", &input.challenge_id, input.ts, &input.device_proof, now) {
            return Err(invalid());
        }
        let vault = state.vault.clone();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let row: Option<(String, String, Vec<u8>, i64, i64, Option<i64>, Option<String>)> = tx
                    .query_row(
                        "SELECT attendee_id, invitation_id, code_mac, attempts, expires_at, consumed_at, session_id
                           FROM event_guest_challenges WHERE id = ?1",
                        params![input.challenge_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                    )
                    .optional()?;
                let (aid, inv, mac, attempts, expires, consumed, session) = row.ok_or_else(invalid)?;
                let att = attendee(tx, &aid)?.ok_or_else(invalid)?;
                if att.removed_at.is_some() || att.redacted_at.is_some() {
                    return Err(invalid());
                }
                let good: bool = {
                    use subtle::ConstantTimeEq;
                    code_mac(&vault, &input.challenge_id, input.code.trim()).ct_eq(&mac).into()
                };
                // A retry of the same successful call returns the same session.
                if let (Some(_), Some(sid)) = (consumed, session) {
                    if !good {
                        return Err(invalid());
                    }
                    let row: Option<(Vec<u8>, i64, Vec<u8>)> = tx
                        .query_row(
                            "SELECT token_sealed, expires_at, device_key FROM event_guest_sessions
                              WHERE id = ?1 AND revoked_at IS NULL AND expires_at > ?2",
                            params![sid, now],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .optional()?;
                    let (sealed, exp, dkey) = row.ok_or_else(invalid)?;
                    if dkey != key {
                        return Err(invalid());
                    }
                    let token = open(&vault, &format!("{sid}:session"), &sealed);
                    return Ok((200, json!({ "guest_session": token, "session_id": sid, "expires_at": exp }), Effects::default()));
                }
                if attempts >= 5 || expires <= now || consumed.is_some() {
                    return Err(invalid());
                }
                tx.execute(
                    "UPDATE event_guest_challenges SET attempts = attempts + 1 WHERE id = ?1",
                    params![input.challenge_id],
                )?;
                if !good {
                    // The attempt counter must survive this refusal: commit it, then refuse.
                    return Ok((403, json!({ "error": "guest_token_invalid", "message": "that code is not right" }), Effects::default()));
                }
                let ev = load_ev(tx, &vault, &att.event_id)?.ok_or_else(invalid)?;
                let token = new_secret();
                let sid = new_id("gss");
                let exp = (now + SESSION_SECS).min(ev.span_to.map(|e| e + JOIN_AFTER_SECS).unwrap_or(i64::MAX)).max(now + 600);
                tx.execute(
                    "INSERT INTO event_guest_sessions (id, token_hash, attendee_id, invitation_id, device_key,
                         attendee_grant_version, verified_at, created_at, expires_at, token_sealed)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
                    params![sid, secret_hash(&token), att.id, inv, key, att.grant_version, now, exp,
                            seal_v(&vault, &format!("{sid}:session"), &token)?],
                )?;
                tx.execute(
                    "UPDATE event_guest_challenges SET consumed_at = ?2, session_id = ?3 WHERE id = ?1",
                    params![input.challenge_id, now, sid],
                )?;
                Ok((200, json!({ "guest_session": token, "session_id": sid, "expires_at": exp }), Effects::default()))
            })
            .await
    }
    .await;
    finish(&state, r)
}

#[derive(Deserialize, Default)]
pub(crate) struct RecoverIn {
    locator: String,
    contact: String,
}

/// `POST /v1/calendar/guest/recover` — always the same answer.
pub(crate) async fn recover(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let fake = new_id("chl");
    let generic = json!({ "ok": true, "challenge_id": fake, "expires_at": now_unix() as i64 + CHALLENGE_SECS });
    let r: GResult<Value> = async {
        crate::groups::rate_limit(&format!("cal-recover:{}", ip_of(&ip)), 5, 3600)?;
        let input: RecoverIn = json_body(body)?;
        let Some(canonical) = canonical_contact(&input.contact) else { return Ok(generic.clone()) };
        let hash = contact_hash(&state.vault, &canonical);
        let vault = state.vault.clone();
        let now = now_unix() as i64;
        let locator = input.locator.trim().to_ascii_uppercase();
        let event_lookup = input.locator.trim().to_string();
        let found = state
            .store
            .group_read(move |c: &Connection| -> GResult<Option<(Att, Ev)>> {
                let ids: Vec<String> = if valid_code(&locator) {
                    let mut stmt = c.prepare(
                        "SELECT e.id FROM events e JOIN meeting_reservations m ON m.id = e.meeting_id
                          WHERE m.code = ?1 AND e.status = 'confirmed'",
                    )?;
                    let rows = stmt.query_map(params![locator], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                } else if valid_event_id(&event_lookup) {
                    vec![event_lookup.clone()]
                } else {
                    Vec::new()
                };
                for id in ids {
                    let Some(ev) = load_ev(c, &vault, &id)? else { continue };
                    if ev.status != "confirmed" || ev.span_to.is_some_and(|e| e < now) {
                        continue;
                    }
                    if let Some(att) = c
                        .query_row(
                            &att_select("event_id = ?1 AND contact_hash = ?2 AND removed_at IS NULL AND redacted_at IS NULL"),
                            params![id, hash],
                            read_att,
                        )
                        .optional()?
                    {
                        return Ok(Some((att, ev)));
                    }
                }
                Ok(None)
            })
            .await?;
        let Some((att, ev)) = found else { return Ok(generic.clone()) };
        if !external_on_for(&state, ev.created_by_account.clone()).await {
            return Ok(generic.clone());
        }
        let vault = state.vault.clone();
        let made = state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<Option<String>> {
                // A fresh link generation backs the challenge; earlier ones keep their own expiry.
                let (_, _) = mint_invite_token(tx, &ev, &att, now as u64)?;
                let tid: String = tx.query_row(
                    "SELECT id FROM event_attendee_tokens WHERE attendee_id = ?1 AND purpose = 'invite_manage'
                      ORDER BY created_at DESC LIMIT 1",
                    params![att.id],
                    |r| r.get(0),
                )?;
                Ok(new_challenge(tx, &vault, &ev, &att, &tid, now).ok())
            })
            .await?;
        state.calendar.nudge.notify_one();
        Ok(match made {
            Some(cid) => json!({ "ok": true, "challenge_id": cid, "expires_at": now + CHALLENGE_SECS }),
            None => generic.clone(),
        })
    }
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) if e.code == "rate_limited" => e.into_response(),
        Err(_) => Json(generic).into_response(),
    }
}

// ---- joining -----------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct JoinIn {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    guest_session: Option<String>,
    occurrence_id: String,
    #[serde(default)]
    device_proof: Option<String>,
    #[serde(default)]
    ts: Option<i64>,
}

/// `POST /v1/calendar/guest/join` → a single-use ticket for the calls service.
pub(crate) async fn join(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-join:{}", ip_of(&ip)), 30, 600)?;
        let input: JoinIn = json_body(body)?;
        let vault = state.vault.clone();
        let now = now_unix() as i64;
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let (att, ev, verified) = match (&input.guest_session, &input.token) {
                    (Some(s), _) => {
                        let (sid, att, ev, key) = by_session(tx, &vault, s, now)?;
                        let proof = input.device_proof.as_deref().ok_or_else(invalid)?;
                        let binding = format!("{sid}\n{}", input.occurrence_id);
                        if !verify_proof(&key, "join", &binding, input.ts.unwrap_or(0), proof, now) {
                            return Err(invalid());
                        }
                        (att, ev, true)
                    }
                    (None, Some(t)) => {
                        let (_, att, ev) = by_token(tx, &vault, t, now)?;
                        (att, ev, false)
                    }
                    _ => return Err(invalid()),
                };
                if ev.status != "confirmed" || ev.meeting_id.is_none() {
                    return Err(GErr::conflict("event_cancelled", "this meeting is not available"));
                }
                let (eid, key) = parse_occurrence_id(&input.occurrence_id).ok_or_else(invalid)?;
                if eid != ev.id {
                    return Err(invalid());
                }
                let occ = occurrence_by_key(tx, &vault, &ev, &key)?.ok_or_else(invalid)?;
                if occ.status != "confirmed" {
                    return Err(GErr::conflict("event_cancelled", "this meeting was cancelled"));
                }
                let s = standing(tx, &att, Some(&occ.id))?;
                if !s.invited {
                    return Err(invalid());
                }
                if s.rsvp == "declined" {
                    return Err(GErr::forbidden("declined", "you declined this meeting; change your answer to join"));
                }
                let (opens, closes) = window(&ev, &occ, extended_until(tx, &occ.id)?);
                if now < opens || now > closes {
                    return Err(GErr::forbidden("window_closed", "the meeting is not open now")
                        .with("opens_at", opens)
                        .with("closes_at", closes));
                }
                let direct = verified && ev.meeting.as_ref().is_some_and(|m| m.waiting == "none");
                let ticket = new_secret();
                tx.execute(
                    "INSERT INTO event_join_tickets (id, token_hash, attendee_id, occurrence_id, meeting_id,
                         verified, grant_version, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![new_id("tkt"), secret_hash(&ticket), att.id, occ.id, ev.meeting_id,
                            verified as i64, att.grant_version, now, (now + TICKET_SECS).min(closes)],
                )?;
                Ok((200, json!({
                    "ticket": ticket, "code": ev.meeting_code, "occurrence_id": occ.id,
                    "admission": if direct { "direct" } else { "knock" },
                    "expires_at": (now + TICKET_SECS).min(closes),
                }), Effects::default()))
            })
            .await
    }
    .await;
    finish(&state, r)
}

// ---- unsubscribe -------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct UnsubscribeIn {
    token: String,
    scope: String,
}

/// `POST /v1/calendar/guest/unsubscribe`
pub(crate) async fn unsubscribe(
    State(state): State<AppState>,
    ip: Option<Extension<crate::ClientIp>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let r = async {
        crate::groups::rate_limit(&format!("cal-guest:{}", ip_of(&ip)), 120, 600)?;
        let input: UnsubscribeIn = json_body(body)?;
        if input.token.len() != 43 || !matches!(input.scope.as_str(), "organizer" | "all") {
            return Err(invalid());
        }
        let now = now_unix() as i64;
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let row: Option<(Option<Vec<u8>>, String)> = tx
                    .query_row(
                        "SELECT a.contact_hash, e.organizer FROM event_attendee_tokens t
                           JOIN event_attendees a ON a.id = t.attendee_id JOIN events e ON e.id = a.event_id
                          WHERE t.token_hash = ?1 AND t.purpose = 'unsubscribe' AND t.expires_at > ?2",
                        params![secret_hash(&input.token), now],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let (hash, organizer) = row.ok_or_else(invalid)?;
                let hash = hash.ok_or_else(invalid)?;
                let scope = if input.scope == "all" { "all".to_string() } else { organizer };
                tx.execute(
                    "INSERT OR IGNORE INTO contact_suppressions (contact_hash, scope, created_at) VALUES (?1, ?2, ?3)",
                    params![hash, scope, now],
                )?;
                Ok((200, json!({ "ok": true, "scope": input.scope }), Effects::default()))
            })
            .await
    }
    .await;
    finish(&state, r)
}

/// Header map is unused by guest routes; kept for symmetry with member routes.
#[allow(dead_code)]
fn _unused(_: &HeaderMap) {}
