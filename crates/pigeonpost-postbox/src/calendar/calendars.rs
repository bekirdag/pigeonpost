//! Calendars, settings, secret feeds, availability and `.ics` import (`docs/calendar.md` §2, §4,
//! §5).

use super::events::{announce, write_ev};
use super::*;
use crate::groups::{parse, IdentityQuery};
use crate::now_unix;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Transaction;
use serde::Deserialize;

fn cal_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "color": r.get::<_, String>(2)?,
        "is_default": r.get::<_, i64>(3)? != 0, "revision": r.get::<_, i64>(4)?,
    }))
}

fn list_calendars(c: &Connection, me: &str) -> GResult<Vec<Value>> {
    let mut stmt = c.prepare(
        "SELECT id, name, color, is_default, revision FROM calendars
          WHERE owner_identity = ?1 AND deleted_at IS NULL ORDER BY is_default DESC, created_at",
    )?;
    let rows = stmt
        .query_map(params![me], cal_json)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `GET /v1/calendars`
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let ctx = match ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let me = ctx.address();
    match state
        .store
        .group_read(move |c: &Connection| -> GResult<Value> {
            Ok(json!({ "calendars": list_calendars(c, &me)? }))
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct CalendarIn {
    #[serde(default)]
    request_key: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default)]
    expected_revision: Option<i64>,
}

fn color_of(c: &Option<String>) -> GResult<Option<String>> {
    match c {
        None => Ok(None),
        Some(c) if COLORS.contains(&c.as_str()) => Ok(Some(c.clone())),
        Some(_) => Err(GErr::bad(
            "bad_request",
            format!("color is one of {}", COLORS.join(", ")),
        )),
    }
}

fn name_of(n: &Option<String>) -> GResult<Option<String>> {
    match n {
        None => Ok(None),
        Some(n) => {
            let n = clean_text(n, 80, false);
            if n.is_empty() {
                Err(GErr::bad("bad_request", "name is required"))
            } else {
                Ok(Some(n))
            }
        }
    }
}

/// `POST /v1/calendars`
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, hash): (CalendarIn, String) = parse(body)?;
        let key = input.request_key.clone().ok_or_else(|| GErr::bad("bad_request", "request_key is required"))?;
        let name = name_of(&input.name)?.ok_or_else(|| GErr::bad("bad_request", "name is required"))?;
        let color = color_of(&input.color)?.unwrap_or_else(|| "blue".into());
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| {
                idempotent(tx, &me, "create_calendar", &key, &hash, now, || {
                    let n: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM calendars WHERE owner_identity = ?1 AND deleted_at IS NULL",
                        params![me],
                        |r| r.get(0),
                    )?;
                    if n >= MAX_CALENDARS {
                        return Err(GErr::bad("calendar_limit", format!("at most {MAX_CALENDARS} calendars")));
                    }
                    let id = new_id("cal");
                    tx.execute(
                        "INSERT INTO calendars (id, owner_identity, name, color, is_default, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![id, me, name, color, (n == 0) as i64, now as i64],
                    )?;
                    let v = tx.query_row(
                        "SELECT id, name, color, is_default, revision FROM calendars WHERE id = ?1",
                        params![id],
                        cal_json,
                    )?;
                    Ok((201, json!({ "calendar": v }), Effects::default()))
                })
            })
            .await
    }
    .await;
    finish(&state, r)
}

fn own_cal_rev(c: &Connection, me: &str, id: &str, expected: Option<i64>) -> GResult<(i64, bool)> {
    let row: Option<(i64, bool)> = c
        .query_row(
            "SELECT revision, is_default FROM calendars WHERE id = ?1 AND owner_identity = ?2 AND deleted_at IS NULL",
            params![id, me],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    let (rev, default) = row.ok_or_else(|| {
        GErr::new(
            StatusCode::NOT_FOUND,
            "calendar_unavailable",
            "no such calendar",
        )
    })?;
    let expected =
        expected.ok_or_else(|| GErr::bad("bad_request", "expected_revision is required"))?;
    if expected != rev {
        return Err(conflict(rev));
    }
    Ok((rev, default))
}

/// `PATCH /v1/calendars/{id}`
pub(crate) async fn patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (CalendarIn, String) = parse(body)?;
        let name = name_of(&input.name)?;
        let color = color_of(&input.color)?;
        let me = ctx.address();
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    own_cal_rev(tx, &me, &id, input.expected_revision)?;
                    if input.is_default == Some(true) {
                        tx.execute(
                            "UPDATE calendars SET is_default = 0, revision = revision + 1
                          WHERE owner_identity = ?1 AND is_default = 1 AND id != ?2",
                            params![me, id],
                        )?;
                        tx.execute(
                            "UPDATE calendars SET is_default = 1 WHERE id = ?1",
                            params![id],
                        )?;
                    }
                    tx.execute(
                    "UPDATE calendars SET name = COALESCE(?2, name), color = COALESCE(?3, color),
                            revision = revision + 1 WHERE id = ?1",
                    params![id, name, color],
                )?;
                    let v = tx.query_row(
                        "SELECT id, name, color, is_default, revision FROM calendars WHERE id = ?1",
                        params![id],
                        cal_json,
                    )?;
                    Ok((200, json!({ "calendar": v }), Effects::default()))
                },
            )
            .await
    }
    .await;
    finish(&state, r)
}

/// `DELETE /v1/calendars/{id}`
pub(crate) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (CalendarIn, String) = parse(body)?;
        let me = ctx.address();
        let account = ctx.account();
        let vault = state.vault.clone();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let (_, default) = own_cal_rev(tx, &me, &id, input.expected_revision)?;
                let others: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM calendars WHERE owner_identity = ?1 AND deleted_at IS NULL AND id != ?2
                          ORDER BY created_at",
                    )?;
                    let rows = stmt.query_map(params![me, id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                };
                if default && others.is_empty() {
                    return Err(GErr::conflict("calendar_in_use", "your only calendar cannot be deleted"));
                }
                let mut fx = Effects::default();
                // Cancel only what the caller organizes here.
                let organized: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM events WHERE calendar_id = ?1 AND organizer = ?2 AND status = 'confirmed'",
                    )?;
                    let rows = stmt.query_map(params![id, me], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                };
                for eid in &organized {
                    if let Some(mut ev) = load_ev(tx, &vault, eid)? {
                        ev.status = "cancelled".into();
                        ev.revision += 1;
                        ev.sequence += 1;
                        write_ev(tx, &vault, &ev, false, now)?;
                        if let Some(m) = &ev.meeting_id {
                            super::events::retire_if_unused(tx, m, now)?;
                        }
                        announce(tx, &ev, "event.cancelled", None, Some("cancel"), account.as_deref(), &mut fx, now)?;
                    }
                }
                // Invitations placed here go back to the default calendar.
                tx.execute("UPDATE event_attendees SET calendar_id = NULL WHERE calendar_id = ?1", params![id])?;
                tx.execute(
                    "UPDATE calendar_feed_grants SET revoked_at = ?2 WHERE calendar_id = ?1 AND revoked_at IS NULL",
                    params![id, now as i64],
                )?;
                tx.execute(
                    "UPDATE calendars SET deleted_at = ?2, is_default = 0, revision = revision + 1 WHERE id = ?1",
                    params![id, now as i64],
                )?;
                if default {
                    tx.execute("UPDATE calendars SET is_default = 1 WHERE id = ?1", params![others[0]])?;
                }
                Ok((200, json!({ "deleted": id, "cancelled_events": organized.len() }), fx))
            })
            .await
    }
    .await;
    finish(&state, r)
}

// ---- settings ----------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct SettingsIn {
    #[serde(default)]
    previews: Option<bool>,
    #[serde(default, deserialize_with = "crate::ephemeral::double_option")]
    timezone: Option<Option<String>>,
    #[serde(default)]
    week_start: Option<i64>,
    #[serde(default)]
    working_hours: Option<Value>,
}

fn settings_json(c: &Connection, me: &str) -> GResult<Value> {
    let row: Option<(i64, Option<String>, i64, Option<String>)> = c
        .query_row(
            "SELECT previews, timezone, week_start, working_hours FROM calendar_settings WHERE mailbox = ?1",
            params![me],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (p, tz, ws, wh) = row.unwrap_or((0, None, 1, None));
    Ok(json!({
        "previews": p != 0, "timezone": tz, "week_start": ws,
        "working_hours": wh.and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({ "start": "09:00", "end": "18:00", "days": [1, 2, 3, 4, 5] })),
    }))
}

/// `GET /v1/calendar/settings`
pub(crate) async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let ctx = match ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let me = ctx.address();
    match state
        .store
        .group_read(move |c: &Connection| settings_json(c, &me))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

fn valid_hhmm(s: &str) -> bool {
    s.len() == 5 && chrono::NaiveTime::parse_from_str(s, "%H:%M").is_ok()
}

/// `PUT /v1/calendar/settings`
pub(crate) async fn put_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (SettingsIn, String) = parse(body)?;
        if let Some(Some(tz)) = &input.timezone {
            time::parse_tz(tz).map_err(time_err)?;
        }
        if let Some(w) = input.week_start {
            if !(0..=6).contains(&w) {
                return Err(GErr::bad("bad_request", "week_start is 0 (Sunday) to 6"));
            }
        }
        let hours = match &input.working_hours {
            None => None,
            Some(v) => {
                let ok = v["start"].as_str().is_some_and(valid_hhmm)
                    && v["end"].as_str().is_some_and(valid_hhmm)
                    && v["start"].as_str() < v["end"].as_str()
                    && v["days"].as_array().is_some_and(|d| {
                        d.len() <= 7
                            && d.iter()
                                .all(|x| x.as_i64().is_some_and(|n| (0..=6).contains(&n)))
                    });
                if !ok {
                    return Err(GErr::bad(
                        "bad_request",
                        "working_hours is {start:\"HH:MM\", end:\"HH:MM\", days:[0-6]}",
                    ));
                }
                Some(json!({ "start": v["start"], "end": v["end"], "days": v["days"] }).to_string())
            }
        };
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    tx.execute(
                        "INSERT INTO calendar_settings (mailbox, updated_at) VALUES (?1, ?2)
                     ON CONFLICT(mailbox) DO NOTHING",
                        params![me, now as i64],
                    )?;
                    if let Some(p) = input.previews {
                        tx.execute(
                            "UPDATE calendar_settings SET previews = ?2 WHERE mailbox = ?1",
                            params![me, p as i64],
                        )?;
                    }
                    if let Some(tz) = &input.timezone {
                        tx.execute(
                            "UPDATE calendar_settings SET timezone = ?2 WHERE mailbox = ?1",
                            params![me, tz],
                        )?;
                    }
                    if let Some(w) = input.week_start {
                        tx.execute(
                            "UPDATE calendar_settings SET week_start = ?2 WHERE mailbox = ?1",
                            params![me, w],
                        )?;
                    }
                    if let Some(h) = &hours {
                        tx.execute(
                            "UPDATE calendar_settings SET working_hours = ?2 WHERE mailbox = ?1",
                            params![me, h],
                        )?;
                    }
                    tx.execute(
                        "UPDATE calendar_settings SET updated_at = ?2 WHERE mailbox = ?1",
                        params![me, now as i64],
                    )?;
                    Ok((200, settings_json(tx, &me)?, Effects::default()))
                },
            )
            .await
    }
    .await;
    finish(&state, r)
}

pub(crate) fn user_tz(c: &Connection, me: &str) -> Tz {
    c.query_row(
        "SELECT timezone FROM calendar_settings WHERE mailbox = ?1",
        params![me],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .and_then(|t| time::parse_tz(&t).ok())
    .unwrap_or(Tz::UTC)
}

// ---- feeds -------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct FeedIn {
    enabled: bool,
    #[serde(default)]
    projection: Option<String>,
}

const FEED_LIFETIME: i64 = 365 * 86_400;
const FEED_PAST: i64 = 90 * 86_400;
const FEED_FUTURE: i64 = 365 * 86_400;

/// `POST /v1/calendar/feeds/{calendar_id}`
pub(crate) async fn set_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(calendar_id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        if !ctx.human() {
            return Err(GErr::forbidden("human_required", "only a person signed in themselves can share a calendar feed"));
        }
        let (input, _): (FeedIn, String) = parse(body)?;
        let projection = match input.projection.as_deref().unwrap_or("personal_redacted") {
            "busy" => "busy",
            "personal_redacted" => "personal_redacted",
            _ => return Err(GErr::bad("bad_request", "projection is busy or personal_redacted")),
        };
        crate::groups::rate_limit(&format!("cal-feed:{}", ctx.address()), 10, 3600)?;
        let me = ctx.address();
        let base = state.calendar.postbox_url.clone();
        let now = now_unix() as i64;
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                if !own_calendar(tx, &me, &calendar_id)? {
                    return Err(GErr::new(StatusCode::NOT_FOUND, "calendar_unavailable", "no such calendar"));
                }
                tx.execute(
                    "UPDATE calendar_feed_grants SET revoked_at = ?2 WHERE calendar_id = ?1 AND revoked_at IS NULL",
                    params![calendar_id, now],
                )?;
                if !input.enabled {
                    return Ok((200, json!({ "enabled": false }), Effects::default()));
                }
                let secret = new_secret();
                tx.execute(
                    "INSERT INTO calendar_feed_grants (id, calendar_id, exporter_identity, token_hash, projection,
                         created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![new_id("feed"), calendar_id, me, secret_hash(&secret), projection, now, now + FEED_LIFETIME],
                )?;
                Ok((200, json!({ "enabled": true, "projection": projection,
                                  "url": format!("{base}/v1/calendar/feed/{secret}.ics"),
                                  "expires_at": now + FEED_LIFETIME }), Effects::default()))
            })
            .await
    }
    .await;
    finish(&state, r)
}

/// `GET /v1/calendar/feeds/{calendar_id}`
pub(crate) async fn get_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(calendar_id): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let ctx = match ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let me = ctx.address();
    let now = now_unix() as i64;
    let r = state
        .store
        .group_read(move |c: &Connection| -> GResult<Value> {
            if !own_calendar(c, &me, &calendar_id)? {
                return Err(GErr::new(
                    StatusCode::NOT_FOUND,
                    "calendar_unavailable",
                    "no such calendar",
                ));
            }
            let row: Option<(String, i64, i64)> = c
                .query_row(
                    "SELECT projection, created_at, expires_at FROM calendar_feed_grants
                      WHERE calendar_id = ?1 AND revoked_at IS NULL AND expires_at > ?2",
                    params![calendar_id, now],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            Ok(match row {
                Some((p, c, e)) => {
                    json!({ "enabled": true, "projection": p, "created_at": c, "expires_at": e })
                }
                None => json!({ "enabled": false }),
            })
        })
        .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /v1/calendar/feed/{secret}.ics` — no bearer; the secret is the grant.
pub(crate) async fn feed(State(state): State<AppState>, Path(secret): Path<String>) -> Response {
    let secret = secret.strip_suffix(".ics").unwrap_or(&secret).to_string();
    let generic = || (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
    if secret.len() != 43 || unb64url(&secret).is_none() {
        return generic();
    }
    let vault = state.vault.clone();
    let inbox = state.calendar.inbox_url.clone();
    let now = now_unix() as i64;
    let hash = secret_hash(&secret);
    let grant: Option<(String, String, String, String)> = match state
        .store
        .group_read(move |c: &Connection| -> GResult<_> {
            Ok(c.query_row(
                "SELECT g.id, g.calendar_id, g.exporter_identity, g.projection FROM calendar_feed_grants g
                   JOIN calendars k ON k.id = g.calendar_id
                   JOIN identities i ON i.address = g.exporter_identity
                  WHERE g.token_hash = ?1 AND g.revoked_at IS NULL AND g.expires_at > ?2
                    AND k.deleted_at IS NULL AND k.owner_identity = g.exporter_identity",
                params![hash, now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?)
        })
        .await
    {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };
    let Some((grant_id, calendar_id, owner, projection)) = grant else {
        return generic();
    };
    // Current export authority: the owner's account still has the calendar feature.
    let account = state
        .store
        .get(owner.clone())
        .await
        .ok()
        .flatten()
        .and_then(|i| i.account_id);
    if !feature_on(&state, account.as_deref(), capabilities::CALENDAR).await {
        return generic();
    }
    if let Err(e) = crate::groups::rate_limit(&format!("cal-feed-get:{grant_id}"), 60, 3600) {
        return e.into_response();
    }
    let r = state
        .store
        .group_read(move |c: &Connection| -> GResult<String> {
            feed_text(c, &vault, &calendar_id, &owner, &projection, now, &inbox)
        })
        .await;
    match r {
        Ok(text) => (
            [
                (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
                (header::CACHE_CONTROL, "private, no-store"),
                (header::REFERRER_POLICY, "no-referrer"),
            ],
            text,
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

fn feed_text(
    c: &Connection,
    vault: &crate::vault::Vault,
    calendar_id: &str,
    owner: &str,
    projection: &str,
    now: i64,
    _inbox: &str,
) -> GResult<String> {
    let (from, to) = (now - FEED_PAST, now + FEED_FUTURE);
    let mut out = String::new();
    let line = |s: String, out: &mut String| super::ics::fold(&s, out);
    line("BEGIN:VCALENDAR".into(), &mut out);
    line(
        "PRODID:-//Pigeonpost//Calendar feed 1.0//EN".into(),
        &mut out,
    );
    line("VERSION:2.0".into(), &mut out);
    line("CALSCALE:GREGORIAN".into(), &mut out);
    line("X-WR-CALNAME:Pigeonpost".into(), &mut out);
    let is_default: bool = c.query_row(
        "SELECT is_default FROM calendars WHERE id = ?1",
        params![calendar_id],
        |r| r.get::<_, i64>(0),
    )? != 0;
    // Organized events in this calendar, and accepted/tentative invitations placed in it.
    let mut ids: Vec<(String, bool)> = Vec::new();
    {
        let mut stmt = c.prepare(
            "SELECT id FROM events WHERE calendar_id = ?1 AND organizer = ?2 AND status = 'confirmed'
               AND span_from < ?4 AND (span_to IS NULL OR span_to > ?3) LIMIT 2000",
        )?;
        for id in stmt.query_map(params![calendar_id, owner, from, to], |r| {
            r.get::<_, String>(0)
        })? {
            ids.push((id?, false));
        }
        let mut stmt = c.prepare(
            "SELECT a.event_id FROM event_attendees a JOIN events e ON e.id = a.event_id
              WHERE a.address = ?1 AND a.removed_at IS NULL AND a.suppressed = 0 AND a.filtered = 0
                AND a.rsvp IN ('accepted','tentative') AND e.status = 'confirmed'
                AND (a.calendar_id = ?2 OR (a.calendar_id IS NULL AND ?3))
                AND e.span_from < ?5 AND (e.span_to IS NULL OR e.span_to > ?4) LIMIT 2000",
        )?;
        for id in stmt.query_map(params![owner, calendar_id, is_default, from, to], |r| {
            r.get::<_, String>(0)
        })? {
            ids.push((id?, true));
        }
    }
    let mut count = 0usize;
    for (id, invitation) in ids {
        let Some(ev) = load_ev(c, vault, &id)? else {
            continue;
        };
        let Ok(occs) = occurrences_in(c, vault, &ev, from, to, 2000) else {
            continue;
        };
        let busy = projection == "busy" || ev.privacy == "private" || invitation;
        for o in occs {
            if o.status != "confirmed" || ev.transparency == "transparent" && busy {
                continue;
            }
            count += 1;
            if count > 5000 {
                break;
            }
            line("BEGIN:VEVENT".into(), &mut out);
            line(format!("UID:{}@pigeonpost.dev", o.id), &mut out);
            line(format!("DTSTAMP:{}", super::ics::utc_stamp(now)), &mut out);
            if o.all_day {
                line(
                    format!("DTSTART;VALUE=DATE:{}", time::key_date(o.start_date())),
                    &mut out,
                );
                line(
                    format!("DTEND;VALUE=DATE:{}", time::key_date(o.end_date())),
                    &mut out,
                );
            } else {
                line(
                    format!("DTSTART:{}", super::ics::utc_stamp(o.starts_at)),
                    &mut out,
                );
                line(
                    format!("DTEND:{}", super::ics::utc_stamp(o.ends_at)),
                    &mut out,
                );
            }
            if busy {
                line("SUMMARY:Busy".into(), &mut out);
                line("CLASS:CONFIDENTIAL".into(), &mut out);
            } else {
                line(format!("SUMMARY:{}", super::ics::esc(&o.title)), &mut out);
                if !o.location.is_empty() {
                    line(
                        format!("LOCATION:{}", super::ics::esc(&o.location)),
                        &mut out,
                    );
                }
            }
            line(
                format!(
                    "TRANSP:{}",
                    if ev.transparency == "transparent" {
                        "TRANSPARENT"
                    } else {
                        "OPAQUE"
                    }
                ),
                &mut out,
            );
            line("END:VEVENT".into(), &mut out);
        }
    }
    line("END:VCALENDAR".into(), &mut out);
    Ok(out)
}

// ---- availability ------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct FreeBusyIn {
    attendees: Vec<String>,
    from: i64,
    to: i64,
}

/// Busy intervals of one mailbox in `[from, to)`: organized opaque events and accepted/tentative
/// opaque invitations; never titles.
pub(crate) fn busy_of(
    c: &Connection,
    vault: &crate::vault::Vault,
    mailbox: &str,
    from: i64,
    to: i64,
) -> GResult<Vec<(i64, i64)>> {
    let mut out = Vec::new();
    let v = super::events::list_in(c, vault, mailbox, from, to, None, false, "")?;
    for o in v["occurrences"].as_array().cloned().unwrap_or_default() {
        if o["transparency"] == "transparent" || o["all_day"] == true {
            continue;
        }
        if o["invited"] == true && !matches!(o["my_rsvp"].as_str(), Some("accepted" | "tentative"))
        {
            continue;
        }
        if let (Some(s), Some(e)) = (o["starts_at"].as_i64(), o["ends_at"].as_i64()) {
            out.push((s.max(from), e.min(to)));
        }
    }
    out.sort();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (s, e) in out {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    Ok(merged)
}

/// `POST /v1/calendar/freebusy` — in this phase only the caller's own mailboxes resolve.
pub(crate) async fn freebusy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (FreeBusyIn, String) = parse(body)?;
        if input.to <= input.from || input.to - input.from > time::MAX_WINDOW_SECS {
            return Err(GErr::bad(
                "window_too_large",
                "ask for at most 62 days at a time",
            ));
        }
        if input.attendees.len() > 50 {
            return Err(GErr::bad("bad_request", "at most 50 mailboxes"));
        }
        crate::groups::rate_limit(&format!("cal-fb:{}", ctx.address()), 60, 60)?;
        let mine: HashSet<String> = match ctx.account() {
            Some(a) => state
                .store
                .list_by_account(a)
                .await
                .map_err(GErr::from)?
                .into_iter()
                .map(|i| i.address)
                .collect(),
            None => [ctx.address()].into_iter().collect(),
        };
        let vault = state.vault.clone();
        let list = input.attendees.clone();
        let (from, to) = (input.from, input.to);
        let v = state
            .store
            .group_read(move |c: &Connection| -> GResult<Value> {
                let mut busy = serde_json::Map::new();
                for a in list {
                    if mine.contains(&a) {
                        let b = busy_of(c, &vault, &a, from, to)?;
                        busy.insert(
                            a,
                            json!(b
                                .iter()
                                .map(|(s, e)| json!({ "start": s, "end": e }))
                                .collect::<Vec<_>>()),
                        );
                    } else {
                        busy.insert(a, json!("unknown"));
                    }
                }
                Ok(json!({ "busy": busy, "from": from, "to": to }))
            })
            .await?;
        Ok((200, v, Effects::default()))
    }
    .await;
    finish(&state, r)
}

// ---- import ------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct PreviewQuery {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    calendar_id: Option<String>,
}

/// `POST /v1/calendar/import/preview` — raw `text/calendar` body.
pub(crate) async fn import_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PreviewQuery>,
    body: Bytes,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        crate::groups::rate_limit(&format!("cal-import:{}", ctx.address()), 20, 3600)?;
        if body.len() > super::ics::MAX_IMPORT_BYTES {
            return Err(GErr::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "the file is larger than 512 KiB"));
        }
        let me = ctx.address();
        let vault = state.vault.clone();
        let now = now_unix() as i64;
        let calendar = q.calendar_id.clone();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let calendar_id = match calendar {
                    Some(c) if own_calendar(tx, &me, &c)? => c,
                    Some(_) => return Err(GErr::new(StatusCode::NOT_FOUND, "calendar_unavailable", "no such calendar")),
                    None => default_calendar(tx, &me, now as u64)?,
                };
                let tz = user_tz(tx, &me);
                let (events, warnings) = super::ics::parse_import(&body, tz).map_err(|why| GErr::bad("import_invalid", why))?;
                let id = new_id("imp");
                let sealed = seal_v(&vault, &format!("{id}:draft"), &serde_json::to_string(&events).expect("serializes"))?;
                tx.execute(
                    "INSERT INTO calendar_import_drafts (id, mailbox, calendar_id, payload_sealed, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, me, calendar_id, sealed, now, now + 3600],
                )?;
                Ok((200, json!({ "draft_id": id, "calendar_id": calendar_id, "events": events,
                                  "warnings": warnings, "expires_at": now + 3600 }), Effects::default()))
            })
            .await
    }
    .await;
    finish(&state, r)
}

#[derive(Deserialize, Default)]
pub(crate) struct ConfirmIn {
    select: Vec<usize>,
    #[serde(default)]
    calendar_id: Option<String>,
}

/// `POST /v1/calendar/import/{draft}/confirm`
pub(crate) async fn import_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(draft): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (ConfirmIn, String) = parse(body)?;
        let me = ctx.address();
        let account = ctx.account();
        let vault = state.vault.clone();
        let inbox = state.calendar.inbox_url.clone();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let row: Option<(String, Vec<u8>)> = tx
                    .query_row(
                        "SELECT calendar_id, payload_sealed FROM calendar_import_drafts
                          WHERE id = ?1 AND mailbox = ?2 AND expires_at > ?3",
                        params![draft, me, now as i64],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let (draft_cal, sealed) = row.ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such draft"))?;
                let events: Vec<super::ics::Imported> =
                    serde_json::from_str(&open(&vault, &format!("{draft}:draft"), &sealed)).unwrap_or_default();
                let calendar_id = input.calendar_id.clone().unwrap_or(draft_cal);
                if !own_calendar(tx, &me, &calendar_id)? {
                    return Err(GErr::new(StatusCode::NOT_FOUND, "calendar_unavailable", "no such calendar"));
                }
                let mut created = Vec::new();
                let mut fx = Effects::default();
                for idx in input.select.iter().take(200) {
                    let Some(imp) = events.iter().find(|e| e.index == *idx && e.importable) else { continue };
                    let ein = super::events::EventIn {
                        title: Some(imp.title.clone()),
                        description: Some(imp.description.clone()),
                        location: Some(imp.location.clone()),
                        all_day: Some(imp.all_day),
                        start: imp.start.clone(),
                        end: imp.end.clone(),
                        tzid: imp.tzid.clone(),
                        start_date: imp.start_date.clone(),
                        end_date: imp.end_date.clone(),
                        rrule: Some(imp.rrule.clone()),
                        exdates: Some(imp.exdates.clone()),
                        fold: Some("earlier".into()),
                        ..super::events::EventIn::default()
                    };
                    let Ok(timing) = super::events::timing_from(&ein, None) else { continue };
                    let ev = super::events::new_plain_event(tx, &vault, &me, &calendar_id, account.clone(), &ein, &timing, now)?;
                    if let Some(uid) = &imp.uid {
                        // Provenance only: never matched to overwrite anything.
                        tx.execute("UPDATE events SET import_uid = ?2 WHERE id = ?1", params![ev.id, uid])?;
                    }
                    tx.execute(
                        "INSERT INTO event_reminders (id, event_id, owner_identity, minutes_before, channel)
                         VALUES (?1, ?2, ?3, 10, 'push')",
                        params![new_id("rem"), ev.id, me],
                    )?;
                    let view = super::events::event_view(tx, &vault, &ev, &me, &super::events::Access::Organizer, &inbox)?;
                    created.push(view);
                }
                tx.execute("DELETE FROM calendar_import_drafts WHERE id = ?1", params![draft])?;
                fx.event(tx, &me, "event.updated", json!({ "imported": created.len() }), now)?;
                Ok((200, json!({ "created": created }), fx))
            })
            .await
    }
    .await;
    finish(&state, r)
}
