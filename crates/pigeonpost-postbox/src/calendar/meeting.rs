//! `POST /internal/v1/calendar/meeting-access` — the calls service asks who may enter a scheduled
//! meeting room, in what role and how (SDS §14B.7, `docs/calendar.md` §8).
//!
//! Roles come only from current event state: the organizer is the host; co-hosts are handle
//! attendees the organizer appointed; invited attendees and verified guests follow the waiting
//! policy; anyone else may only knock, and only when the event allows it. Being first in the room
//! grants nothing. Refusals are `200 {allowed:false, reason}` so the calls service can tell "no"
//! from "cannot ask" (which it treats as closed).

use super::*;
use crate::now_unix;
use crate::policy::Action;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Transaction;
use serde::Deserialize;

#[derive(Deserialize, Debug, Default)]
pub(crate) struct MailboxIn {
    address: String,
    #[serde(default)]
    #[allow(dead_code)]
    handle: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub(crate) struct ActorIn {
    kind: String,
    #[serde(default)]
    mailboxes: Vec<MailboxIn>,
    #[serde(default)]
    ticket: Option<String>,
    #[serde(default)]
    attendee_id: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub(crate) struct AccessIn {
    code: String,
    #[serde(default)]
    occurrence_id: Option<String>,
    actor: ActorIn,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    extend_minutes: Option<i64>,
}

fn deny(reason: &str) -> Value {
    json!({ "allowed": false, "reason": reason })
}

/// `POST /internal/v1/calendar/meeting-access`
pub(crate) async fn meeting_access(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::CalendarMeetingAccess,
        Action::MeetingAccess,
    ) {
        return *resp;
    }
    let Ok(Json(v)) = body else {
        return GErr::bad("bad_request", "malformed request").into_response();
    };
    let Ok(input) = serde_json::from_value::<AccessIn>(v) else {
        return GErr::bad("bad_request", "malformed request").into_response();
    };
    let code = input.code.trim().replace('-', "").to_ascii_uppercase();
    if !valid_code(&code) || input.actor.mailboxes.len() > 200 {
        return Json(deny("unavailable")).into_response();
    }
    // The calendar feature must be on for the event's organizer; checked inside via the account.
    let vault = state.vault.clone();
    let inbox = state.calendar.inbox_url.clone();
    let now = now_unix() as i64;
    let r = state
        .store
        .group_tx(move |tx: &Transaction<'_>| -> GResult<Value> {
            decide(tx, &vault, &inbox, &code, &input, now)
        })
        .await;
    match r {
        Ok(v) => {
            // A disabled feature looks like an unknown code.
            if v["allowed"] == true {
                let account = v["_account"].as_str().map(str::to_string);
                if !feature_on(&state, account.as_deref(), capabilities::CALENDAR).await {
                    return Json(deny("unavailable")).into_response();
                }
            }
            let mut v = v;
            if let Value::Object(m) = &mut v {
                m.remove("_account");
            }
            Json(v).into_response()
        }
        Err(e) => e.into_response(),
    }
}

struct Cand {
    ev: Ev,
    occ: Occ,
    opens: i64,
    closes: i64,
}

fn decide(
    tx: &Connection,
    vault: &crate::vault::Vault,
    _inbox: &str,
    code: &str,
    input: &AccessIn,
    now: i64,
) -> GResult<Value> {
    let purpose = input.purpose.as_deref().unwrap_or("join");
    let mid: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT id, retired_at FROM meeting_reservations WHERE code = ?1",
            params![code],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((meeting_id, _retired)) = mid else {
        return Ok(deny("unavailable"));
    };
    let mut events = Vec::new();
    let mut any_cancelled = false;
    {
        let mut stmt = tx.prepare("SELECT id, status FROM events WHERE meeting_id = ?1")?;
        for row in stmt.query_map(params![meeting_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (id, status) = row?;
            if status == "confirmed" {
                if let Some(ev) = load_ev(tx, vault, &id)? {
                    events.push(ev);
                }
            } else {
                any_cancelled = true;
            }
        }
    }
    // Occurrences near now (open ones, and the next ones for "not open yet" answers).
    let mut open: Vec<Cand> = Vec::new();
    let mut upcoming: Vec<Cand> = Vec::new();
    for ev in &events {
        let Ok(occs) = occurrences_in(
            tx,
            vault,
            ev,
            now - MAX_EXTENSION_SECS - JOIN_AFTER_SECS - 86_400,
            now + 2 * 86_400,
            200,
        ) else {
            continue;
        };
        for occ in occs {
            if occ.status != "confirmed" {
                continue;
            }
            let (opens, closes) = window(ev, &occ, extended_until(tx, &occ.id)?);
            let c = Cand {
                ev: ev.clone(),
                occ,
                opens,
                closes,
            };
            if now >= opens && now <= closes {
                open.push(c);
            } else if opens > now {
                upcoming.push(c);
            }
        }
    }
    open.sort_by_key(|c| c.occ.starts_at);
    upcoming.sort_by_key(|c| c.occ.starts_at);
    let occ_list = |list: &[Cand]| -> Value {
        Value::Array(
            list.iter()
                .take(5)
                .map(|c| json!({ "occurrence_id": c.occ.id, "starts_at": c.occ.starts_at, "ends_at": c.occ.ends_at,
                                 "opens_at": c.opens, "closes_at": c.closes }))
                .collect(),
        )
    };

    // Anonymous link holders learn only whether they may knock right now.
    if input.actor.kind == "anonymous" {
        let Some(c) = open
            .iter()
            .find(|c| c.ev.meeting.as_ref().is_some_and(|m| m.allow_knock))
        else {
            return Ok(deny("unavailable"));
        };
        let mut v = base(c, &meeting_id, code, tx)?;
        v["role"] = json!("none");
        v["admission"] = json!("knock");
        v["can_admit"] = json!(false);
        v["participant"] = json!({ "name": null, "verified": false });
        return Ok(v);
    }

    // Pick the occurrence.
    let chosen: Option<&Cand> = match &input.occurrence_id {
        Some(id) => {
            let found = open.iter().find(|c| &c.occ.id == id);
            if found.is_none() {
                let known = events
                    .iter()
                    .any(|e| parse_occurrence_id(id).is_some_and(|(eid, _)| eid == e.id));
                return Ok(if known {
                    let mut v = deny("window_closed");
                    v["occurrences"] = occ_list(&upcoming);
                    v
                } else {
                    deny("unavailable")
                });
            }
            found
        }
        None => match open.len() {
            0 => None,
            1 => open.first(),
            _ => {
                let mut v = deny("choose_occurrence");
                v["occurrences"] = occ_list(&open);
                return Ok(v);
            }
        },
    };

    match input.actor.kind.as_str() {
        "member" => member(
            tx,
            vault,
            chosen,
            &events,
            &upcoming,
            any_cancelled,
            &meeting_id,
            code,
            input,
            purpose,
            now,
            occ_list,
        ),
        "guest" => guest(tx, vault, chosen, &meeting_id, code, input, purpose, now),
        _ => Ok(deny("unavailable")),
    }
}

/// The fields every positive answer carries.
fn base(c: &Cand, meeting_id: &str, code: &str, tx: &Connection) -> GResult<Value> {
    let ev = &c.ev;
    let m = ev.meeting.clone().unwrap_or_else(MeetingOpts::defaults);
    let mut admitters = vec![ev.organizer.clone()];
    for a in live_attendees(tx, &ev.id)? {
        let Some(addr) = &a.address else { continue };
        if a.suppressed {
            continue;
        }
        let s = standing(tx, &a, Some(&c.occ.id))?;
        if !s.invited {
            continue;
        }
        let cohost = s.role == "cohost";
        let allowed = match m.admit.as_str() {
            "organizer" => false,
            "hosts" => cohost,
            _ => cohost || m.admitters.contains(&a.id),
        };
        if allowed && !admitters.contains(addr) {
            admitters.push(addr.clone());
        }
    }
    Ok(json!({
        "allowed": true, "reason": null,
        "meeting_id": meeting_id, "code": code, "event_id": ev.id, "occurrence_id": c.occ.id,
        "starts_at": c.occ.starts_at, "ends_at": c.occ.ends_at, "opens_at": c.opens, "closes_at": c.closes,
        "organizer": person(tx, &ev.organizer)?,
        "policy": { "waiting": m.waiting, "admit": m.admit, "allow_knock": m.allow_knock,
                    "participant_limit": ev.participant_limit, "transport": ev.transport },
        "admitters": admitters,
        "policy_version": format!("{}.{}", ev.policy_version, ev.revision),
        "event_revision": ev.revision,
        "_account": ev.created_by_account,
        "occurrences": [],
    }))
}

#[allow(clippy::too_many_arguments)]
fn member(
    tx: &Connection,
    _vault: &crate::vault::Vault,
    chosen: Option<&Cand>,
    events: &[Ev],
    upcoming: &[Cand],
    _any_cancelled: bool,
    meeting_id: &str,
    code: &str,
    input: &AccessIn,
    purpose: &str,
    now: i64,
    occ_list: impl Fn(&[Cand]) -> Value,
) -> GResult<Value> {
    let mailboxes: Vec<&str> = input
        .actor
        .mailboxes
        .iter()
        .map(|m| m.address.as_str())
        .collect();
    if mailboxes.is_empty() {
        return Ok(deny("unavailable"));
    }
    // Is this person in the audience at all (for "not open yet" vs "not invited")?
    let in_audience = |ev: &Ev| -> GResult<bool> {
        for mb in &mailboxes {
            if ev.organizer == *mb {
                return Ok(true);
            }
            if my_attendance(tx, &ev.id, mb)?.is_some_and(|a| !a.suppressed) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let Some(c) = chosen else {
        let invited = events
            .iter()
            .map(&in_audience)
            .collect::<GResult<Vec<bool>>>()?
            .into_iter()
            .any(|x| x);
        return Ok(if invited {
            let mut v = deny("window_closed");
            v["occurrences"] = occ_list(upcoming);
            v
        } else {
            deny("unavailable")
        });
    };
    let ev = &c.ev;
    let m = ev.meeting.clone().unwrap_or_else(MeetingOpts::defaults);
    // Strongest role among the caller's mailboxes.
    let mut best: Option<(u8, String, Option<Att>, &str)> = None; // (rank, address, attendee, role)
    let mut declined = false;
    for mb in &mailboxes {
        if ev.organizer == *mb {
            best = Some((3, mb.to_string(), None, "host"));
            break;
        }
        if let Some(a) = my_attendance(tx, &ev.id, mb)? {
            if a.suppressed || a.filtered {
                continue;
            }
            let s = standing(tx, &a, Some(&c.occ.id))?;
            if !s.invited {
                continue;
            }
            if s.rsvp == "declined" {
                declined = true;
                continue;
            }
            let (rank, role) = if s.role == "cohost" {
                (2, "cohost")
            } else {
                (1, "attendee")
            };
            if best.as_ref().is_none_or(|b| b.0 < rank) {
                best = Some((rank, mb.to_string(), Some(a), role));
            }
        }
    }
    let mut v = base(c, meeting_id, code, tx)?;
    let Some((_, address, att, role)) = best else {
        if declined {
            return Ok(deny("declined"));
        }
        if m.allow_knock && purpose != "extend" {
            v["role"] = json!("none");
            v["admission"] = json!("knock");
            v["can_admit"] = json!(false);
            let addr = mailboxes[0];
            v["participant"] = json!({ "address": addr, "handle": handle_of(tx, addr)?,
                                       "name": handle_of(tx, addr)?.unwrap_or_else(|| addr.to_string()),
                                       "verified": true });
            return Ok(v);
        }
        return Ok(deny("not_invited"));
    };
    let admission = match role {
        "host" | "cohost" => "direct",
        _ if m.waiting == "everyone_except_hosts" => "knock",
        _ => "direct",
    };
    let can_admit = v["admitters"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x == address.as_str()));
    if purpose == "extend" {
        if role != "host" {
            return Ok(deny("not_allowed"));
        }
        let minutes = input.extend_minutes.unwrap_or(30).clamp(5, 60);
        ensure_occurrence(tx, ev, &c.occ)?;
        let max = c.occ.ends_at + MAX_EXTENSION_SECS;
        let next = (c.closes.max(now) + minutes * 60).min(max);
        tx.execute(
            "UPDATE event_occurrences SET extended_until = ?2 WHERE id = ?1",
            params![c.occ.id, next],
        )?;
        v["closes_at"] = json!(next);
    }
    let handle = handle_of(tx, &address)?;
    v["role"] = json!(role);
    v["admission"] = json!(admission);
    v["can_admit"] = json!(can_admit);
    v["participant"] = json!({
        "address": address, "handle": handle, "attendee_id": att.as_ref().map(|a| a.id.clone()),
        "name": handle.clone().unwrap_or_else(|| address.clone()), "verified": true,
    });
    Ok(v)
}

#[allow(clippy::too_many_arguments)]
fn guest(
    tx: &Connection,
    _vault: &crate::vault::Vault,
    chosen: Option<&Cand>,
    meeting_id: &str,
    code: &str,
    input: &AccessIn,
    purpose: &str,
    now: i64,
) -> GResult<Value> {
    let (att, verified) = match (purpose, &input.actor.ticket, &input.actor.attendee_id) {
        ("recheck", _, Some(aid)) => {
            let Some(a) = attendee(tx, aid)? else {
                return Ok(deny("not_invited"));
            };
            (a, true)
        }
        (_, Some(ticket), _) => {
            let row: Option<(String, String, String, String, i64, i64)> = tx
                .query_row(
                    "SELECT id, attendee_id, occurrence_id, meeting_id, verified, grant_version FROM event_join_tickets
                      WHERE token_hash = ?1 AND redeemed_at IS NULL AND expires_at >= ?2",
                    params![secret_hash(ticket), now],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let Some((tid, aid, occ, mid, verified, grant)) = row else {
                return Ok(deny("ticket_invalid"));
            };
            if mid != meeting_id || chosen.is_some_and(|c| c.occ.id != occ) {
                return Ok(deny("ticket_invalid"));
            }
            tx.execute(
                "UPDATE event_join_tickets SET redeemed_at = ?2 WHERE id = ?1",
                params![tid, now],
            )?;
            let Some(a) = attendee(tx, &aid)? else {
                return Ok(deny("ticket_invalid"));
            };
            if a.grant_version != grant {
                return Ok(deny("ticket_invalid"));
            }
            (a, verified != 0)
        }
        _ => return Ok(deny("ticket_invalid")),
    };
    let Some(c) = chosen else {
        return Ok(deny("window_closed"));
    };
    if att.event_id != c.ev.id
        || att.removed_at.is_some()
        || att.redacted_at.is_some()
        || att.kind != "external"
    {
        return Ok(deny("not_invited"));
    }
    let s = standing(tx, &att, Some(&c.occ.id))?;
    if !s.invited {
        return Ok(deny("not_invited"));
    }
    if s.rsvp == "declined" {
        return Ok(deny("declined"));
    }
    let m = c.ev.meeting.clone().unwrap_or_else(MeetingOpts::defaults);
    let mut v = base(c, meeting_id, code, tx)?;
    v["role"] = json!("guest");
    v["admission"] = json!(if verified && m.waiting == "none" {
        "direct"
    } else {
        "knock"
    });
    v["can_admit"] = json!(false);
    v["participant"] = json!({
        "attendee_id": att.id, "name": att.display_name.clone().unwrap_or_else(|| "Guest".into()),
        "verified": verified,
    });
    Ok(v)
}
