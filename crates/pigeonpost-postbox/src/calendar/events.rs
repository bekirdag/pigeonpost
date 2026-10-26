//! Event and occurrence routes for members (SDS §14B.5, `docs/calendar.md` §3).

use super::time::{self, Fold};
use super::*;
use crate::groups::{parse, IdentityQuery};
use crate::now_unix;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Transaction;
use serde::Deserialize;

// ---- input -------------------------------------------------------------------------------------

#[derive(Deserialize, Default, Clone, Debug)]
pub(crate) struct AttendeeIn {
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub contact: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub optional: Option<bool>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub(crate) struct MeetingIn {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub waiting: Option<String>,
    #[serde(default)]
    pub admit: Option<String>,
    #[serde(default)]
    pub allow_knock: Option<bool>,
    #[serde(default)]
    pub join_before_minutes: Option<i64>,
    #[serde(default)]
    pub guests_see_list: Option<bool>,
    #[serde(default)]
    pub transport: Option<String>,
    /// Handles (or attendee ids) allowed to admit when `admit = delegated`.
    #[serde(default)]
    pub admitters: Option<Vec<String>>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub(crate) struct ReminderIn {
    pub minutes_before: i64,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub(crate) struct EventIn {
    #[serde(default)]
    pub request_key: Option<String>,
    #[serde(default)]
    pub calendar_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub all_day: Option<bool>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub tzid: Option<String>,
    #[serde(default)]
    pub fold: Option<String>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default, deserialize_with = "crate::ephemeral::double_option")]
    pub rrule: Option<Option<String>>,
    #[serde(default)]
    pub exdates: Option<Vec<String>>,
    #[serde(default)]
    pub transparency: Option<String>,
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub attendees: Option<Vec<AttendeeIn>>,
    #[serde(default)]
    pub meeting: Option<MeetingIn>,
    #[serde(default)]
    pub reminders: Option<Vec<ReminderIn>>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub occurrence_id: Option<String>,
}

impl EventIn {
    fn touches_time(&self) -> bool {
        self.all_day.is_some()
            || self.start.is_some()
            || self.end.is_some()
            || self.tzid.is_some()
            || self.start_date.is_some()
            || self.end_date.is_some()
    }
}

/// Validated timing.
#[derive(Debug, Clone)]
pub(crate) struct Timing {
    pub all_day: bool,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub local_start: Option<NaiveDateTime>,
    pub local_end: Option<NaiveDateTime>,
    pub tz: Option<Tz>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

impl Timing {
    pub(crate) fn of(ev: &Ev) -> Self {
        Timing {
            all_day: ev.all_day,
            start_date: ev.start_date,
            end_date: ev.end_date,
            local_start: ev.local_start,
            local_end: ev.local_end,
            tz: if ev.all_day { None } else { Some(ev.tz()) },
            starts_at: ev.starts_at,
            ends_at: ev.ends_at,
        }
    }
    pub(crate) fn series_start(&self) -> NaiveDateTime {
        if self.all_day {
            time::midnight(self.start_date.unwrap_or_default())
        } else {
            self.local_start.unwrap_or_default()
        }
    }
    fn same_as(&self, other: &Timing) -> bool {
        self.all_day == other.all_day
            && self.start_date == other.start_date
            && self.end_date == other.end_date
            && self.local_start == other.local_start
            && self.local_end == other.local_end
            && self.tz.map(|t| t.name()) == other.tz.map(|t| t.name())
    }
}

/// Read a timing from input, starting from `base` (for edits).
pub(crate) fn timing_from(input: &EventIn, base: Option<&Timing>) -> GResult<Timing> {
    let all_day = input.all_day.or(base.map(|b| b.all_day)).unwrap_or(false);
    let fold = match input.fold.as_deref() {
        None => None,
        Some(f) => Some(
            Fold::parse(f).ok_or_else(|| GErr::bad("bad_request", "fold is earlier or later"))?,
        ),
    };
    if all_day {
        let start = match (&input.start_date, base) {
            (Some(s), _) => time::parse_date(s).map_err(time_err)?,
            (None, Some(b)) if b.all_day => b.start_date.unwrap_or_default(),
            (None, Some(b)) => b.local_start.map(|l| l.date()).unwrap_or_default(),
            (None, None) => {
                return Err(
                    GErr::bad("time_invalid", "start_date is required").with("kind", "invalid")
                )
            }
        };
        let end = match (&input.end_date, base) {
            (Some(e), _) => time::parse_date(e).map_err(time_err)?,
            (None, Some(b)) if b.all_day && input.start_date.is_none() => {
                b.end_date.unwrap_or(start + Duration::days(1))
            }
            _ => start + Duration::days(1),
        };
        time::validate_dates(start, end).map_err(time_err)?;
        return Ok(Timing {
            all_day: true,
            start_date: Some(start),
            end_date: Some(end),
            local_start: None,
            local_end: None,
            tz: None,
            starts_at: None,
            ends_at: None,
        });
    }
    let tz = match (&input.tzid, base.and_then(|b| b.tz)) {
        (Some(t), _) => time::parse_tz(t).map_err(time_err)?,
        (None, Some(t)) => t,
        (None, None) => return Err(GErr::bad("zone_unknown", "tzid is required")),
    };
    let start = match (&input.start, base.and_then(|b| b.local_start)) {
        (Some(s), _) => time::parse_local(s).map_err(time_err)?,
        (None, Some(s)) => s,
        (None, None) => {
            return Err(GErr::bad("time_invalid", "start is required").with("kind", "invalid"))
        }
    };
    let end = match (&input.end, base.and_then(|b| b.local_end)) {
        (Some(e), _) => time::parse_local(e).map_err(time_err)?,
        // Moving the start keeps the length.
        (None, Some(e)) => {
            let old_start = base.and_then(|b| b.local_start).unwrap_or(start);
            start + (e - old_start)
        }
        (None, None) => start + Duration::minutes(30),
    };
    let (s, e) = time::validate_timed(start, end, tz, fold).map_err(time_err)?;
    Ok(Timing {
        all_day: false,
        start_date: None,
        end_date: None,
        local_start: Some(start),
        local_end: Some(end),
        tz: Some(tz),
        starts_at: Some(s),
        ends_at: Some(e),
    })
}

fn one_of(field: &str, value: &str, allowed: &[&str]) -> GResult<String> {
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(GErr::bad(
            "bad_request",
            format!("{field} must be one of {}", allowed.join(", ")),
        ))
    }
}

/// Validate meeting options against the event and its attendees.
fn meeting_opts(m: &MeetingIn, base: Option<&MeetingOpts>) -> GResult<(MeetingOpts, String)> {
    let mut o = base.cloned().unwrap_or_else(MeetingOpts::defaults);
    if let Some(w) = &m.waiting {
        o.waiting = one_of(
            "meeting.waiting",
            w,
            &["guests", "everyone_except_hosts", "none"],
        )?;
    }
    if let Some(a) = &m.admit {
        o.admit = one_of("meeting.admit", a, &["organizer", "hosts", "delegated"])?;
    }
    if let Some(k) = m.allow_knock {
        o.allow_knock = k;
    }
    if let Some(b) = m.join_before_minutes {
        if !(0..=30).contains(&b) {
            return Err(GErr::bad(
                "bad_request",
                "meeting.join_before_minutes must be 0–30",
            ));
        }
        o.join_before_minutes = b;
    }
    if let Some(g) = m.guests_see_list {
        o.guests_see_list = g;
    }
    let transport = match m.transport.as_deref() {
        None => "mesh".to_string(),
        Some(t) => one_of("meeting.transport", t, &["mesh", "sfu"])?,
    };
    Ok((o, transport))
}

pub(crate) fn limit_for(transport: &str) -> i64 {
    if transport == "sfu" {
        SFU_LIMIT
    } else {
        MESH_LIMIT
    }
}

// ---- attendee judging (outside the transaction) -------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) enum Who {
    Handle {
        address: String,
        filtered: bool,
        suppressed: bool,
    },
    External {
        canonical: String,
        hash: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Judged {
    pub who: Who,
    pub name: Option<String>,
    pub role: String,
    pub optional: bool,
}

/// Resolve handles and judge each against the recipient's admission settings, as messages are
/// (SDS §14B.3): blocked → recorded but never delivered (`suppressed`), filtered → `filtered`.
pub(crate) async fn judge(
    state: &AppState,
    organizer: &store::StoredIdentity,
    list: &[AttendeeIn],
) -> GResult<Vec<Judged>> {
    let mut out = Vec::new();
    let mut seen_handles = HashSet::new();
    let mut seen_contacts = HashSet::new();
    let mut unknown = Vec::new();
    for a in list {
        let role = match a.role.as_deref() {
            None | Some("attendee") => "attendee".to_string(),
            Some("cohost") => "cohost".to_string(),
            Some(_) => return Err(GErr::bad("bad_request", "role is attendee or cohost")),
        };
        let optional = a.optional.unwrap_or(false);
        let name = a.name.as_deref().map(clean_name).filter(|n| !n.is_empty());
        match (&a.handle, &a.contact) {
            (Some(h), None) => {
                let h = h.trim();
                let found = crate::resolve_recipient(state, h).await.unwrap_or_default();
                let Some(recipient) = found else {
                    unknown.push(h.to_string());
                    continue;
                };
                if recipient.address == organizer.address
                    || !seen_handles.insert(recipient.address.clone())
                {
                    continue;
                }
                let rel = state
                    .store
                    .relationship(
                        recipient.address.clone(),
                        organizer.address.clone(),
                        organizer.handle.clone(),
                    )
                    .await
                    .map_err(GErr::from)?;
                let (mut filtered, mut suppressed) = (false, rel.blocked);
                if !suppressed {
                    if feature_on(
                        state,
                        recipient.account_id.as_deref(),
                        capabilities::MESSAGE_FILTERING,
                    )
                    .await
                    {
                        let filters = state
                            .store
                            .filters(recipient.address.clone())
                            .await
                            .map_err(GErr::from)?;
                        let score = crate::sender_score(state, organizer).await;
                        match crate::filtering::message_rule(
                            &filters,
                            &rel,
                            organizer.handle.is_none(),
                            score,
                        ) {
                            store::MessageRule::Show => {}
                            store::MessageRule::Filter => filtered = true,
                            store::MessageRule::Block => suppressed = true,
                        }
                    } else if !rel.known() {
                        filtered = !rel.allowed;
                    }
                }
                out.push(Judged {
                    who: Who::Handle {
                        address: recipient.address,
                        filtered,
                        suppressed,
                    },
                    name: None,
                    role,
                    optional,
                });
            }
            (None, Some(c)) => {
                if role == "cohost" {
                    return Err(GErr::bad(
                        "bad_request",
                        "only people with a Pigeonpost handle can be co-hosts",
                    ));
                }
                let canonical = canonical_contact(c).ok_or_else(|| {
                    GErr::bad("bad_request", "that is not a valid contact address")
                })?;
                if !seen_contacts.insert(canonical.clone()) {
                    continue;
                }
                let hash = contact_hash(&state.vault, &canonical);
                out.push(Judged {
                    who: Who::External { canonical, hash },
                    name,
                    role,
                    optional,
                });
            }
            _ => {
                return Err(GErr::bad(
                    "bad_request",
                    "each attendee is {handle} or {contact, name?}",
                ))
            }
        }
    }
    if !unknown.is_empty() {
        return Err(
            GErr::bad("attendee_unknown", "no Pigeonpost mailbox answers to these")
                .with("unknown", unknown),
        );
    }
    Ok(out)
}

/// Insert one judged attendee (scope all, or one occurrence) inside the transaction, queueing what
/// it needs. Returns the attendee id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_attendee(
    tx: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    j: &Judged,
    only_occurrence: Option<&Occ>,
    account: Option<&str>,
    fx: &mut Effects,
    now: u64,
) -> GResult<String> {
    let id = new_id("att");
    let base = if only_occurrence.is_some() {
        "excluded"
    } else {
        "invited"
    };
    match &j.who {
        Who::Handle {
            address,
            filtered,
            suppressed,
        } => {
            tx.execute(
                "INSERT INTO event_attendees (id, event_id, kind, address, role, optional,
                     base_participation, filtered, suppressed, added_at)
                 VALUES (?1, ?2, 'handle', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    ev.id,
                    address,
                    j.role,
                    j.optional as i64,
                    base,
                    *filtered as i64,
                    *suppressed as i64,
                    now as i64
                ],
            )?;
            // Their own reminder, ten minutes before (it fires only while they are eligible).
            tx.execute(
                "INSERT INTO event_reminders (id, event_id, attendee_id, minutes_before, channel)
                 VALUES (?1, ?2, ?3, 10, 'push')",
                params![new_id("rem"), ev.id, id],
            )?;
            if !*suppressed {
                fx.event(
                    tx,
                    address,
                    "event.invite",
                    json!({ "event_id": ev.id, "revision": ev.revision, "filtered": *filtered,
                            "occurrence_id": only_occurrence.map(|o| o.id.clone()) }),
                    now,
                )?;
                if !*filtered {
                    fx.invitation_push(address, &ev.organizer, &ev.id);
                }
            }
        }
        Who::External { canonical, hash } => {
            let sealed = seal_v(vault, &format!("{id}:contact"), canonical)?;
            tx.execute(
                "INSERT INTO event_attendees (id, event_id, kind, contact_hash, contact_sealed,
                     display_name, role, optional, base_participation, added_at)
                 VALUES (?1, ?2, 'external', ?3, ?4, ?5, 'attendee', ?6, ?7, ?8)",
                params![
                    id,
                    ev.id,
                    hash,
                    sealed,
                    j.name,
                    j.optional as i64,
                    base,
                    now as i64
                ],
            )?;
            super::worker::queue(
                tx,
                ev,
                &id,
                "invite",
                only_occurrence.map(|o| o.id.as_str()),
                1,
                now,
                account,
                now,
            )?;
            fx.nudge = true;
        }
    }
    if let Some(occ) = only_occurrence {
        ensure_occurrence(tx, ev, occ)?;
        tx.execute(
            "INSERT INTO event_occurrence_attendees (occurrence_id, attendee_id, participation)
             VALUES (?1, ?2, 'invited')
             ON CONFLICT(occurrence_id, attendee_id) DO UPDATE SET participation = 'invited',
                revision = revision + 1",
            params![occ.id, id],
        )?;
    }
    Ok(id)
}

/// Invitations to contact addresses this account queued in the last day.
fn external_invites_today(c: &Connection, account: Option<&str>, now: u64) -> GResult<i64> {
    let Some(account) = account else { return Ok(0) };
    Ok(c.query_row(
        "SELECT COUNT(*) FROM calendar_delivery_outbox
          WHERE account_id = ?1 AND workflow = 'invite' AND created_at > ?2",
        params![account, now.saturating_sub(86_400) as i64],
        |r| r.get(0),
    )?)
}

/// The gate for anything that sends a new invitation to a contact address.
fn external_gate(state_ctx: &Ctx, organizer_score: i64, n: usize) -> GResult<()> {
    if n == 0 {
        return Ok(());
    }
    if !state_ctx.human_with_verified_address() {
        return Err(GErr::forbidden(
            "human_required",
            "inviting people by contact address needs you signed in yourself, with a verified sign-in address",
        ));
    }
    if organizer_score < 0 {
        return Err(GErr::forbidden(
            "invite_limit",
            "invitations to contact addresses are paused for this mailbox",
        ));
    }
    Ok(())
}

fn capacity_check(transport: &str, limit: i64, attendees: usize) -> GResult<()> {
    let needed = attendees as i64 + 1;
    if needed > limit {
        return Err(GErr::bad(
            "meeting_capacity",
            format!(
                "a {} meeting holds {limit} people including you; invite fewer people, make it a large call, or turn the meeting off",
                if transport == "sfu" { "large" } else { "regular" }
            ),
        )
        .with("participant_limit", limit));
    }
    Ok(())
}

/// Reserve a fresh meeting code. Codes are never reassigned (retired ones keep their row).
pub(crate) fn reserve_code(c: &Connection, creator: &str, now: u64) -> GResult<(String, String)> {
    for _ in 0..8 {
        let id = new_id("mtg");
        let code = new_code();
        let n = c.execute(
            "INSERT OR IGNORE INTO meeting_reservations (id, code, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, code, creator, now as i64],
        )?;
        if n == 1 {
            return Ok((id, code));
        }
    }
    Err(GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "busy",
        "try again",
    ))
}

// ---- writing events ----------------------------------------------------------------------------

/// Derived span used by listings: first start (UTC; all-day: UTC midnight) to last end, or open.
fn span_of(series: &time::Series) -> (i64, Option<i64>) {
    let from = series
        .first()
        .map(|i| i.starts_at)
        .unwrap_or_else(|| series.instance_at(series.start).starts_at);
    let to = if series.rule.is_some() {
        series.last_end()
    } else {
        Some(series.instance_at(series.start).ends_at)
    };
    (from, to)
}

/// Write every column of `ev` (insert or full update).
pub(crate) fn write_ev(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    insert: bool,
    now: u64,
) -> GResult<()> {
    let title = seal_v(vault, &format!("{}:title", ev.id), &ev.title)?;
    let details = seal_v(
        vault,
        &format!("{}:details", ev.id),
        &json!({ "description": ev.description, "location": ev.location }).to_string(),
    )?;
    let series = ev.series();
    let (span_from, span_to) = span_of(&series);
    let exdates = (!ev.exdates.is_empty()).then(|| ev.exdates.join(","));
    let meeting = ev
        .meeting
        .as_ref()
        .map(|m| serde_json::to_string(m).expect("serializes"));
    let (sd, ed) = (
        ev.start_date.map(time::fmt_date),
        ev.end_date.map(time::fmt_date),
    );
    let (ls, le) = (
        ev.local_start.map(time::fmt_local),
        ev.local_end.map(time::fmt_local),
    );
    let vtype = if ev.all_day { "date" } else { "date_time" };
    if insert {
        c.execute(
            "INSERT INTO events (id, calendar_id, organizer, revision, sequence, title_sealed,
                details_sealed, value_type, start_date, end_date_exclusive, local_start, local_end,
                tzid, starts_at, ends_at, tzdb_version, rrule, exdates, recur_until, transparency,
                privacy, meeting_id, meeting_options, meeting_policy_version, transport_profile,
                participant_limit, linked_conversation, locale, split_from, status,
                created_by_account, created_at, updated_at, span_from, span_to)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,
                     ?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?32,?33,?34)",
            params![
                ev.id,
                ev.calendar_id,
                ev.organizer,
                ev.revision,
                ev.sequence,
                title,
                details,
                vtype,
                sd,
                ed,
                ls,
                le,
                ev.tzid,
                ev.starts_at,
                ev.ends_at,
                time::tzdb_version(),
                ev.rrule,
                exdates,
                span_to,
                ev.transparency,
                ev.privacy,
                ev.meeting_id,
                meeting,
                ev.policy_version,
                ev.transport,
                ev.participant_limit,
                ev.linked_group,
                ev.locale,
                ev.split_from,
                ev.status,
                ev.created_by_account,
                now as i64,
                span_from,
                span_to
            ],
        )?;
    } else {
        c.execute(
            "UPDATE events SET calendar_id = ?2, revision = ?3, sequence = ?4, title_sealed = ?5,
                details_sealed = ?6, value_type = ?7, start_date = ?8, end_date_exclusive = ?9,
                local_start = ?10, local_end = ?11, tzid = ?12, starts_at = ?13, ends_at = ?14,
                tzdb_version = ?15, rrule = ?16, exdates = ?17, recur_until = ?18,
                transparency = ?19, privacy = ?20, meeting_id = ?21, meeting_options = ?22,
                meeting_policy_version = ?23, transport_profile = ?24, participant_limit = ?25,
                linked_conversation = ?26, locale = ?27, status = ?28, updated_at = ?29,
                span_from = ?30, span_to = ?31
              WHERE id = ?1",
            params![
                ev.id,
                ev.calendar_id,
                ev.revision,
                ev.sequence,
                title,
                details,
                vtype,
                sd,
                ed,
                ls,
                le,
                ev.tzid,
                ev.starts_at,
                ev.ends_at,
                time::tzdb_version(),
                ev.rrule,
                exdates,
                span_to,
                ev.transparency,
                ev.privacy,
                ev.meeting_id,
                meeting,
                ev.policy_version,
                ev.transport,
                ev.participant_limit,
                ev.linked_group,
                ev.locale,
                ev.status,
                now as i64,
                span_from,
                span_to
            ],
        )?;
    }
    Ok(())
}

fn apply_timing(ev: &mut Ev, t: &Timing) {
    ev.all_day = t.all_day;
    ev.start_date = t.start_date;
    ev.end_date = t.end_date;
    ev.local_start = t.local_start;
    ev.local_end = t.local_end;
    ev.tzid = t.tz.map(|z| z.name().to_string());
    ev.starts_at = t.starts_at;
    ev.ends_at = t.ends_at;
}

/// Recompute `starts_at`/`ends_at` as the first generated occurrence (DTSTART may not match the
/// rule; the stored values are the first real occurrence's, per §14B.4).
fn settle_first(ev: &mut Ev) {
    if ev.all_day {
        return;
    }
    if let Some(first) = ev.series().first() {
        ev.starts_at = Some(first.starts_at);
        ev.ends_at = Some(first.ends_at);
    }
}

fn text_fields(ev: &mut Ev, input: &EventIn) -> GResult<()> {
    if let Some(t) = &input.title {
        let t = clean_text(t, MAX_TITLE, false);
        if t.is_empty() {
            return Err(GErr::bad("bad_request", "title is required"));
        }
        ev.title = t;
    }
    if let Some(d) = &input.description {
        ev.description = clean_text(d, MAX_DESCRIPTION, true);
    }
    if let Some(l) = &input.location {
        ev.location = clean_text(l, MAX_LOCATION, false);
    }
    if let Some(t) = &input.transparency {
        ev.transparency = one_of("transparency", t, &["opaque", "transparent"])?;
    }
    if let Some(p) = &input.privacy {
        ev.privacy = one_of("privacy", p, &["default", "private"])?;
    }
    if let Some(l) = &input.locale {
        ev.locale = one_of("locale", l, &["en", "tr"])?;
    }
    Ok(())
}

fn rule_fields(ev: &mut Ev, input: &EventIn) -> GResult<()> {
    let start = Timing::of(ev).series_start();
    if let Some(r) = &input.rrule {
        ev.rrule = match r.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(body) => Some(
                time::parse_rule(body, ev.all_day, start)
                    .map_err(|why| GErr::bad("recurrence_invalid", why))?
                    .body(),
            ),
        };
    } else if let Some(body) = ev.rrule.clone() {
        // The start or kind changed: the existing rule must still hold (UNTIL type follows it).
        ev.rrule = Some(
            time::parse_rule(&body, ev.all_day, start)
                .map_err(|why| GErr::bad("recurrence_invalid", why))?
                .body(),
        );
    }
    if let Some(x) = &input.exdates {
        if x.len() > 1000 {
            return Err(GErr::bad("recurrence_invalid", "too many exdates"));
        }
        let mut keys = Vec::new();
        for k in x {
            let ok = if ev.all_day {
                time::parse_key_date(k).is_some()
            } else {
                time::parse_key_timed(k).is_some()
            };
            if !ok {
                return Err(GErr::bad(
                    "recurrence_invalid",
                    "exdates are recurrence keys of the event's own type",
                ));
            }
            keys.push(k.clone());
        }
        keys.sort();
        keys.dedup();
        ev.exdates = keys;
    }
    if ev.rrule.is_none() {
        ev.exdates.clear();
    }
    if ev.series().first().is_none() {
        return Err(GErr::bad(
            "recurrence_invalid",
            "the rule produces no occurrences",
        ));
    }
    Ok(())
}

fn set_reminders(
    c: &Connection,
    event_id: &str,
    owner: Option<&str>,
    attendee: Option<&str>,
    list: &[ReminderIn],
    allow_external: bool,
) -> GResult<()> {
    if list.len() > MAX_REMINDERS {
        return Err(GErr::bad(
            "bad_request",
            format!("at most {MAX_REMINDERS} reminders"),
        ));
    }
    let mut seen = HashSet::new();
    for r in list {
        if !(0..=40_320).contains(&r.minutes_before) {
            return Err(GErr::bad("bad_request", "minutes_before is 0–40320"));
        }
        let channel = r.channel.as_deref().unwrap_or("push");
        if channel != "push" && !(allow_external && channel == "external") {
            return Err(GErr::bad("bad_request", "channel must be push"));
        }
        seen.insert((r.minutes_before, channel.to_string()));
    }
    match (owner, attendee) {
        (Some(o), None) => c.execute(
            "UPDATE event_reminders SET enabled = 0, revision = revision + 1
              WHERE event_id = ?1 AND owner_identity = ?2 AND enabled = 1",
            params![event_id, o],
        )?,
        (None, Some(a)) => c.execute(
            "UPDATE event_reminders SET enabled = 0, revision = revision + 1
              WHERE event_id = ?1 AND attendee_id = ?2 AND enabled = 1",
            params![event_id, a],
        )?,
        _ => 0,
    };
    for (minutes, channel) in seen {
        c.execute(
            "INSERT INTO event_reminders (id, event_id, attendee_id, owner_identity, minutes_before, channel)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id("rem"), event_id, attendee, owner, minutes, channel],
        )?;
    }
    Ok(())
}

fn my_reminders(
    c: &Connection,
    event_id: &str,
    owner: Option<&str>,
    attendee: Option<&str>,
) -> GResult<Value> {
    let mut stmt = c.prepare(
        "SELECT minutes_before, channel FROM event_reminders
          WHERE event_id = ?1 AND enabled = 1 AND (owner_identity = ?2 OR attendee_id = ?3)
          ORDER BY minutes_before",
    )?;
    let rows = stmt
        .query_map(params![event_id, owner, attendee], |r| {
            Ok(json!({ "minutes_before": r.get::<_, i64>(0)?, "channel": r.get::<_, String>(1)? }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Value::Array(rows))
}

// ---- projections -------------------------------------------------------------------------------

/// The caller's relation to an event.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Access {
    Organizer,
    Attendee(Att),
}

pub(crate) fn access(c: &Connection, ev: &Ev, me: &str) -> GResult<Option<Access>> {
    if ev.organizer == me {
        return Ok(Some(Access::Organizer));
    }
    Ok(my_attendance(c, &ev.id, me)?
        .filter(|a| !a.suppressed)
        .map(Access::Attendee))
}

fn delivery_state(c: &Connection, event_id: &str, attendee_id: &str) -> GResult<Option<String>> {
    Ok(c.query_row(
        "SELECT state FROM calendar_delivery_outbox
              WHERE event_id = ?1 AND recipient_id = ?2 AND workflow IN ('invite','update')
                AND state != 'obsolete'
              ORDER BY created_at DESC, rowid DESC LIMIT 1",
        params![event_id, attendee_id],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|s| {
        match s.as_str() {
            "queued" | "leased" | "uncertain" => "queued",
            "accepted" | "delivered" => "delivered",
            "failed" => "failed",
            "suppressed" => "suppressed",
            _ => "queued",
        }
        .to_string()
    }))
}

pub(crate) fn meeting_view(ev: &Ev, inbox: &str, admitters: Vec<String>) -> Value {
    match (&ev.meeting, &ev.meeting_code) {
        (Some(m), Some(code)) => json!({
            "code": code, "url": format!("{inbox}/m/{code}"),
            "waiting": m.waiting, "admit": m.admit, "allow_knock": m.allow_knock,
            "join_before_minutes": m.join_before_minutes, "guests_see_list": m.guests_see_list,
            "transport": ev.transport, "participant_limit": ev.participant_limit,
            "admitters": admitters,
        }),
        _ => Value::Null,
    }
}

pub(crate) fn timing_json(ev: &Ev) -> Value {
    json!({
        "all_day": ev.all_day,
        "start": ev.local_start.map(time::fmt_local),
        "end": ev.local_end.map(time::fmt_local),
        "tzid": ev.tzid,
        "starts_at": ev.starts_at, "ends_at": ev.ends_at,
        "start_date": ev.start_date.map(time::fmt_date),
        "end_date": ev.end_date.map(time::fmt_date),
    })
}

pub(crate) fn occ_json(occ: &Occ) -> Value {
    json!({
        "occurrence_id": occ.id, "recurrence_key": occ.key,
        "all_day": occ.all_day,
        "starts_at": (!occ.all_day).then_some(occ.starts_at),
        "ends_at": (!occ.all_day).then_some(occ.ends_at),
        "start_date": occ.all_day.then(|| time::fmt_date(occ.start_date())),
        "end_date": occ.all_day.then(|| time::fmt_date(occ.end_date())),
        "start": (!occ.all_day).then(|| time::fmt_local(occ.local_start)),
        "end": (!occ.all_day).then(|| time::fmt_local(occ.local_end)),
        "tzid": occ.tzid,
        "status": occ.status, "overridden": occ.overridden,
    })
}

/// The full detail of an event for someone in its audience.
pub(crate) fn event_view(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    me: &str,
    acc: &Access,
    inbox: &str,
) -> GResult<Value> {
    let organizer = matches!(acc, Access::Organizer);
    let atts = live_attendees(c, &ev.id)?;
    let mut list = Vec::new();
    let mut counts: HashMap<&str, i64> = HashMap::new();
    let mut admitters = Vec::new();
    for a in &atts {
        // Someone who blocked the organizer is listed to the organizer exactly like anyone who has
        // not answered (the organizer must not learn of the block); to others, not at all.
        if a.suppressed && !organizer {
            continue;
        }
        let rsvp = if a.suppressed {
            "needs_action"
        } else {
            a.rsvp.as_str()
        };
        *counts
            .entry(match rsvp {
                "accepted" => "accepted",
                "tentative" => "tentative",
                "declined" => "declined",
                _ => "needs_action",
            })
            .or_default() += 1;
        let mut v = json!({
            "attendee_id": a.id, "kind": a.kind, "role": a.role, "optional": a.optional,
            "rsvp": rsvp, "rsvp_at": if a.suppressed { None } else { a.rsvp_at },
            "invited": a.base == "invited",
        });
        if a.kind == "handle" {
            let addr = a.address.clone().unwrap_or_default();
            let handle = handle_of(c, &addr)?;
            v["address"] = json!(addr);
            v["name"] = json!(handle.clone().unwrap_or_else(|| addr.clone()));
            v["handle"] = json!(handle);
            if ev
                .meeting
                .as_ref()
                .is_some_and(|m| m.admitters.contains(&a.id))
            {
                admitters.push(addr);
            }
        } else {
            v["name"] = json!(a.display_name.clone().unwrap_or_else(|| "Guest".into()));
            if organizer {
                v["contact"] = json!(a.contact_sealed.as_ref().map(|b| open(
                    vault,
                    &format!("{}:contact", a.id),
                    b
                )));
                v["proof"] = json!(a.rsvp_proof);
            }
        }
        if organizer {
            let d = if a.kind == "handle" {
                Some("delivered".to_string())
            } else {
                delivery_state(c, &ev.id, &a.id)?
            };
            v["delivery"] = json!(d);
        }
        list.push(v);
    }
    let (my, reminders) = match acc {
        Access::Organizer => (
            json!({ "role": "organizer", "attendee_id": null, "rsvp": null, "calendar_id": ev.calendar_id }),
            my_reminders(c, &ev.id, Some(me), None)?,
        ),
        Access::Attendee(a) => (
            json!({ "role": a.role, "attendee_id": a.id, "rsvp": a.rsvp, "calendar_id": a.calendar_id,
                    "filtered": a.filtered, "invited": a.base == "invited",
                    "note": a.rsvp_note_sealed.as_ref().map(|b| open(vault, &format!("{}:note", a.id), b)) }),
            my_reminders(c, &ev.id, None, Some(&a.id))?,
        ),
    };
    let mut v = json!({
        "id": ev.id, "uid": ev.uid(), "calendar_id": if organizer { Some(ev.calendar_id.clone()) } else { None },
        "revision": ev.revision, "sequence": ev.sequence, "status": ev.status,
        "organizer": person(c, &ev.organizer)?,
        "title": ev.title, "description": ev.description, "location": ev.location,
        "rrule": ev.rrule, "exdates": ev.exdates,
        "transparency": ev.transparency, "privacy": ev.privacy,
        "meeting": meeting_view(ev, inbox, admitters),
        "attendees": list,
        "counts": {
            "accepted": counts.get("accepted").copied().unwrap_or(0),
            "tentative": counts.get("tentative").copied().unwrap_or(0),
            "declined": counts.get("declined").copied().unwrap_or(0),
            "needs_action": counts.get("needs_action").copied().unwrap_or(0),
        },
        "my": my, "reminders": reminders,
        "linked_group": ev.linked_group, "locale": ev.locale,
        "split_from": ev.split_from,
        "can_edit": organizer,
    });
    if let (Value::Object(map), Value::Object(t)) = (&mut v, timing_json(ev)) {
        map.extend(t);
    }
    Ok(v)
}

// ---- handlers: read ----------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct ListQuery {
    #[serde(default)]
    identity: Option<String>,
    from: i64,
    to: i64,
    #[serde(default)]
    calendars: Option<String>,
    #[serde(default)]
    include: Option<String>,
}

/// `GET /v1/calendar/events`
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    q: Result<Query<ListQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(q)) = q else {
        return GErr::bad("bad_request", "from and to (Unix seconds) are required").into_response();
    };
    let ctx = match ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    if q.to <= q.from {
        return GErr::bad("bad_request", "to must be after from").into_response();
    }
    if q.to - q.from > time::MAX_WINDOW_SECS {
        return GErr::bad("window_too_large", "ask for at most 62 days at a time").into_response();
    }
    let me = ctx.address();
    let vault = state.vault.clone();
    let inbox = state.calendar.inbox_url.clone();
    let wanted: Option<HashSet<String>> = q.calendars.as_deref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    });
    let invitations = q
        .include
        .as_deref()
        .is_some_and(|i| i.split(',').any(|x| x == "invitations"));
    let (from, to) = (q.from, q.to);
    let result = state
        .store
        .group_read(move |c: &Connection| -> GResult<Value> {
            list_in(
                c,
                &vault,
                &me,
                from,
                to,
                wanted.as_ref(),
                invitations,
                &inbox,
            )
        })
        .await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn list_in(
    c: &Connection,
    vault: &crate::vault::Vault,
    me: &str,
    from: i64,
    to: i64,
    wanted: Option<&HashSet<String>>,
    invitations: bool,
    inbox: &str,
) -> GResult<Value> {
    let mut calendars: HashMap<String, (String, bool)> = HashMap::new();
    {
        let mut stmt = c.prepare(
            "SELECT id, color, is_default FROM calendars WHERE owner_identity = ?1 AND deleted_at IS NULL",
        )?;
        for row in stmt.query_map(params![me], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
            ))
        })? {
            let (id, color, default) = row?;
            calendars.insert(id, (color, default));
        }
    }
    let default_cal = calendars
        .iter()
        .find(|(_, (_, d))| *d)
        .map(|(id, _)| id.clone());
    let selected = |cal: &str| wanted.is_none_or(|w| w.contains(cal));
    let slack = 14 * 3600;
    let (lo, hi) = (from - slack, to + slack);

    // Candidate events: organized in my calendars, or invitations to me — overlapping the window by
    // span, or with a stored occurrence moved into it.
    let mut candidates: Vec<(String, Option<Att>)> = Vec::new();
    let mut seen = HashSet::new();
    {
        let mut stmt = c.prepare(
            "SELECT e.id FROM events e JOIN calendars k ON k.id = e.calendar_id
              WHERE k.owner_identity = ?1 AND e.organizer = ?1 AND e.status = 'confirmed'
                AND ((e.span_from < ?3 AND (e.span_to IS NULL OR e.span_to > ?2))
                     OR EXISTS (SELECT 1 FROM event_occurrences o
                                 WHERE o.event_id = e.id AND o.starts_at < ?3 AND o.ends_at > ?2))",
        )?;
        for id in stmt.query_map(params![me, lo, hi], |r| r.get::<_, String>(0))? {
            let id = id?;
            if seen.insert(id.clone()) {
                candidates.push((id, None));
            }
        }
    }
    {
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM event_attendees a JOIN events e ON e.id = a.event_id
              WHERE a.address = ?1 AND a.kind = 'handle' AND a.removed_at IS NULL
                AND a.suppressed = 0 AND a.filtered = 0 AND e.status = 'confirmed'
                AND ((e.span_from < ?3 AND (e.span_to IS NULL OR e.span_to > ?2))
                     OR EXISTS (SELECT 1 FROM event_occurrences o
                                 WHERE o.event_id = e.id AND o.starts_at < ?3 AND o.ends_at > ?2))",
            ATT_COLS_A
        ))?;
        for att in stmt.query_map(params![me, lo, hi], read_att)? {
            let att = att?;
            if att.rsvp == "needs_action" && !invitations {
                continue;
            }
            if seen.insert(att.event_id.clone()) {
                candidates.push((att.event_id.clone(), Some(att)));
            }
        }
    }

    let mut out = Vec::new();
    for (id, att) in candidates {
        let Some(ev) = load_ev(c, vault, &id)? else {
            continue;
        };
        let cal = match &att {
            None => ev.calendar_id.clone(),
            Some(a) => a
                .calendar_id
                .clone()
                .or(default_cal.clone())
                .unwrap_or_default(),
        };
        if !selected(&cal) {
            continue;
        }
        let color = calendars
            .get(&cal)
            .map(|(c, _)| c.clone())
            .unwrap_or_else(|| "blue".into());
        let occs = occurrences_in(c, vault, &ev, from, to, time::MAX_OCCURRENCES)?;
        for occ in occs {
            if occ.status != "confirmed" {
                continue;
            }
            let mut role = "organizer".to_string();
            let mut my_rsvp: Option<String> = None;
            if let Some(a) = &att {
                let s = standing(c, a, Some(&occ.id))?;
                if !s.invited {
                    continue;
                }
                if s.rsvp == "needs_action" && !invitations {
                    continue;
                }
                role = s.role.clone();
                my_rsvp = Some(s.rsvp);
            }
            let meeting = match (&ev.meeting, &ev.meeting_code) {
                (Some(_), Some(code)) => {
                    let (opens, closes) = window(&ev, &occ, None);
                    json!({ "code": code, "url": format!("{inbox}/m/{code}"),
                            "opens_at": opens, "closes_at": closes })
                }
                _ => Value::Null,
            };
            let mut v = occ_json(&occ);
            if let Value::Object(m) = &mut v {
                m.insert("event_id".into(), json!(ev.id));
                m.insert("calendar_id".into(), json!(cal));
                m.insert("color".into(), json!(color));
                m.insert("title".into(), json!(occ.title));
                m.insert("location".into(), json!(occ.location));
                m.insert("recurring".into(), json!(ev.recurring()));
                m.insert("organizer".into(), person(c, &ev.organizer)?);
                m.insert("role".into(), json!(role));
                m.insert("my_rsvp".into(), json!(my_rsvp));
                m.insert("invited".into(), json!(att.is_some()));
                m.insert("meeting".into(), meeting);
                m.insert("privacy".into(), json!(ev.privacy));
                m.insert("transparency".into(), json!(ev.transparency));
                m.insert("revision".into(), json!(ev.revision));
            }
            out.push(v);
            if out.len() > time::MAX_OCCURRENCES {
                return Err(GErr::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "expansion_limit",
                    "too many occurrences in this window; ask for a shorter one",
                ));
            }
        }
    }
    out.sort_by_key(|v| {
        (
            v["starts_at"].as_i64().unwrap_or(0),
            v["start_date"].as_str().unwrap_or("").to_string(),
        )
    });
    Ok(json!({ "occurrences": out, "from": from, "to": to }))
}

/// `a.`-qualified attendee columns for joins.
const ATT_COLS_A: &str = "a.id, a.event_id, a.kind, a.address, a.contact_sealed, a.contact_hash,
    a.display_name, a.role, a.optional, a.base_participation, a.rsvp, a.rsvp_note_sealed,
    a.rsvp_at, a.rsvp_proof, a.revision, a.grant_version, a.calendar_id, a.filtered, a.suppressed,
    a.can_admit, a.added_at, a.removed_at, a.redacted_at";

#[derive(Deserialize, Default)]
pub(crate) struct OneQuery {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
}

/// `GET /v1/calendar/events/{id}` (and `{id}.ics`).
pub(crate) async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<OneQuery>,
) -> Response {
    let ctx = match ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let (id, as_ics) = match id.strip_suffix(".ics") {
        Some(base) => (base.to_string(), true),
        None => (id, false),
    };
    if !valid_event_id(&id) {
        return not_found().into_response();
    }
    let me = ctx.address();
    let vault = state.vault.clone();
    let inbox = state.calendar.inbox_url.clone();
    let occurrence = q.occurrence_id.clone();
    let result = state
        .store
        .group_read(move |c: &Connection| -> GResult<(Value, Option<String>)> {
            let ev = load_ev(c, &vault, &id)?.ok_or_else(not_found)?;
            let acc = access(c, &ev, &me)?.ok_or_else(not_found)?;
            if as_ics {
                let att = match &acc {
                    Access::Attendee(a) => Some(a.clone()),
                    Access::Organizer => None,
                };
                let text = super::ics::export(c, &vault, &ev, att.as_ref(), &inbox)?;
                return Ok((Value::Null, Some(text)));
            }
            let mut body = json!({ "event": event_view(c, &vault, &ev, &me, &acc, &inbox)? });
            if let Some(occ_id) = occurrence {
                let (eid, key) = parse_occurrence_id(&occ_id).ok_or_else(not_found)?;
                if eid != ev.id {
                    return Err(not_found());
                }
                let occ = occurrence_by_key(c, &vault, &ev, &key)?.ok_or_else(not_found)?;
                let mut o = occ_json(&occ);
                if let Access::Attendee(a) = &acc {
                    let s = standing(c, a, Some(&occ.id))?;
                    o["my_rsvp"] = json!(s.rsvp);
                    o["invited"] = json!(s.invited);
                }
                if ev.meeting.is_some() {
                    let (opens, closes) = window(&ev, &occ, extended_until(c, &occ.id)?);
                    o["opens_at"] = json!(opens);
                    o["closes_at"] = json!(closes);
                }
                o["title"] = json!(occ.title);
                o["location"] = json!(occ.location);
                o["description"] = json!(occ.description);
                body["occurrence"] = o;
            }
            Ok((body, None))
        })
        .await;
    match result {
        Ok((_, Some(text))) => (
            [
                (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"event.ics\"",
                ),
            ],
            text,
        )
            .into_response(),
        Ok((v, None)) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

// ---- handlers: create --------------------------------------------------------------------------

/// `POST /v1/calendar/events`
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = create_inner(&state, &headers, q.identity.as_deref(), body).await;
    finish(&state, r)
}

async fn create_inner(
    state: &AppState,
    headers: &HeaderMap,
    identity: Option<&str>,
    body: Result<Json<Value>, JsonRejection>,
) -> GResult<(u16, Value, Effects)> {
    let ctx = ctx(state, headers, identity).await?;
    let (input, hash): (EventIn, String) = parse(body)?;
    let key = input
        .request_key
        .clone()
        .ok_or_else(|| GErr::bad("bad_request", "request_key is required"))?;
    crate::groups::rate_limit(&format!("cal-create:{}", ctx.address()), 60, 60)?;
    let timing = timing_from(&input, None)?;
    let title = clean_text(input.title.as_deref().unwrap_or(""), MAX_TITLE, false);
    if title.is_empty() {
        return Err(GErr::bad("bad_request", "title is required"));
    }
    let attendees_in = input.attendees.clone().unwrap_or_default();
    if attendees_in.len() > MAX_HANDLE_ATTENDEES + MAX_EXTERNAL_ATTENDEES {
        return Err(GErr::bad("attendee_limit", "too many attendees"));
    }
    let judged = judge(state, &ctx.me, &attendees_in).await?;
    let handles = judged
        .iter()
        .filter(|j| matches!(j.who, Who::Handle { .. }))
        .count();
    let externals = judged.len() - handles;
    if handles > MAX_HANDLE_ATTENDEES || externals > MAX_EXTERNAL_ATTENDEES {
        return Err(GErr::bad(
            "attendee_limit",
            format!("at most {MAX_HANDLE_ATTENDEES} people with handles and {MAX_EXTERNAL_ATTENDEES} contact addresses per event"),
        ));
    }
    let score = crate::sender_score(state, &ctx.me).await;
    external_gate(&ctx, score, externals)?;

    let meeting_in = input.meeting.clone().unwrap_or_default();
    let meeting = if meeting_in.enabled {
        if timing.all_day {
            return Err(GErr::bad(
                "bad_request",
                "an all-day event cannot be a meeting; make a timed event for the call",
            ));
        }
        let (opts, transport) = meeting_opts(&meeting_in, None)?;
        let non_optional = judged.iter().filter(|j| !j.optional).count();
        capacity_check(&transport, limit_for(&transport), non_optional)?;
        Some((opts, transport))
    } else {
        None
    };
    let reminders = input.reminders.clone();

    let vault = state.vault.clone();
    let inbox = state.calendar.inbox_url.clone();
    let me = ctx.me.clone();
    let account = ctx.account();
    let now = now_unix();
    state
        .store
        .group_tx(move |tx: &Transaction<'_>| {
            idempotent(tx, &vault, &me.address, "create_event", &key, &hash, now, || {
                let organized_today: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events WHERE organizer = ?1 AND created_at > ?2",
                    params![me.address, now.saturating_sub(86_400) as i64],
                    |r| r.get(0),
                )?;
                if organized_today >= EVENTS_PER_DAY as i64 {
                    return Err(GErr::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many events today"));
                }
                if externals > 0
                    && external_invites_today(tx, account.as_deref(), now)? + externals as i64
                        > EXTERNAL_INVITES_PER_DAY
                {
                    return Err(GErr::new(
                        StatusCode::TOO_MANY_REQUESTS,
                        "invite_limit",
                        format!("at most {EXTERNAL_INVITES_PER_DAY} invitations to contact addresses a day"),
                    ));
                }
                let calendar_id = match &input.calendar_id {
                    Some(id) => {
                        if !own_calendar(tx, &me.address, id)? {
                            return Err(GErr::new(StatusCode::NOT_FOUND, "calendar_unavailable", "no such calendar"));
                        }
                        id.clone()
                    }
                    None => default_calendar(tx, &me.address, now)?,
                };
                let mut ev = blank_event(&me.address, &calendar_id, account.clone(), &input, &timing)?;
                ev.title = title.clone();
                if let Some((opts, transport)) = &meeting {
                    let (mid, code) = reserve_code(tx, &me.address, now)?;
                    ev.meeting_id = Some(mid);
                    ev.meeting_code = Some(code);
                    ev.meeting = Some(opts.clone());
                    ev.transport = transport.clone();
                    ev.participant_limit = limit_for(transport);
                }
                write_ev(tx, &vault, &ev, true, now)?;
                let mut fx = Effects::default();
                let mut ids = Vec::new();
                for j in &judged {
                    ids.push((j.clone(), insert_attendee(tx, &vault, &ev, j, None, account.as_deref(), &mut fx, now)?));
                }
                // Delegated admitters by handle.
                if let (Some(opts), Some(names)) = (ev.meeting.as_mut(), meeting_in.admitters.as_ref()) {
                    opts.admitters = resolve_admitters(tx, &ev.id, names)?;
                    write_ev(tx, &vault, &ev, false, now)?;
                }
                match &reminders {
                    Some(list) => set_reminders(tx, &ev.id, Some(&me.address), None, list, false)?,
                    None => set_reminders(
                        tx,
                        &ev.id,
                        Some(&me.address),
                        None,
                        &[ReminderIn { minutes_before: 10, channel: None }],
                        false,
                    )?,
                }
                fx.event(tx, &me.address, "event.updated", json!({ "event_id": ev.id, "revision": ev.revision }), now)?;
                let ev = load_ev(tx, &vault, &ev.id)?.ok_or_else(not_found)?;
                let view = event_view(tx, &vault, &ev, &me.address, &Access::Organizer, &inbox)?;
                Ok((201, json!({ "event": view, "delivery_status": "queued" }), fx))
            })
        })
        .await
}

/// Attendee ids for admitter names (handles, addresses or attendee ids) among the live handle
/// attendees.
fn resolve_admitters(c: &Connection, event_id: &str, names: &[String]) -> GResult<Vec<String>> {
    let atts = live_attendees(c, event_id)?;
    let mut out = Vec::new();
    for n in names {
        let n = n.trim().to_ascii_lowercase();
        let found = atts.iter().find(|a| {
            a.kind == "handle"
                && (a.id == n
                    || a.address.as_deref().map(str::to_ascii_lowercase) == Some(n.clone())
                    || handle_of(c, a.address.as_deref().unwrap_or(""))
                        .ok()
                        .flatten()
                        .map(|h| h.to_ascii_lowercase())
                        == Some(n.clone()))
        });
        match found {
            Some(a) => {
                if !out.contains(&a.id) {
                    out.push(a.id.clone())
                }
            }
            None => {
                return Err(GErr::bad(
                    "bad_request",
                    "meeting.admitters must be invited attendees with a handle",
                ))
            }
        }
    }
    Ok(out)
}

/// A new event (not yet written) from validated input: no attendees, no meeting.
pub(crate) fn blank_event(
    organizer: &str,
    calendar_id: &str,
    account: Option<String>,
    input: &EventIn,
    timing: &Timing,
) -> GResult<Ev> {
    let mut ev = Ev {
        id: new_id("evt"),
        calendar_id: calendar_id.to_string(),
        organizer: organizer.to_string(),
        revision: 1,
        sequence: 0,
        title: "Untitled".into(),
        description: String::new(),
        location: String::new(),
        all_day: false,
        start_date: None,
        end_date: None,
        local_start: None,
        local_end: None,
        tzid: None,
        starts_at: None,
        ends_at: None,
        rrule: None,
        exdates: Vec::new(),
        transparency: "opaque".into(),
        privacy: "default".into(),
        meeting_id: None,
        meeting_code: None,
        meeting: None,
        policy_version: 1,
        transport: "mesh".into(),
        participant_limit: MESH_LIMIT,
        linked_group: None,
        locale: "en".into(),
        status: "confirmed".into(),
        created_by_account: account,
        split_from: None,
        span_from: 0,
        span_to: None,
    };
    apply_timing(&mut ev, timing);
    text_fields(&mut ev, input)?;
    rule_fields(&mut ev, input)?;
    settle_first(&mut ev);
    Ok(ev)
}

/// Create and write a plain event (used by import).
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_plain_event(
    tx: &Connection,
    vault: &crate::vault::Vault,
    organizer: &str,
    calendar_id: &str,
    account: Option<String>,
    input: &EventIn,
    timing: &Timing,
    now: u64,
) -> GResult<Ev> {
    let ev = blank_event(organizer, calendar_id, account, input, timing)?;
    write_ev(tx, vault, &ev, true, now)?;
    Ok(ev)
}

// ---- handlers: change --------------------------------------------------------------------------

fn scope_of(input: &Option<String>) -> GResult<&'static str> {
    match input.as_deref().unwrap_or("all") {
        "all" => Ok("all"),
        "this" => Ok("this"),
        "following" => Ok("following"),
        _ => Err(GErr::bad("bad_request", "scope is all, this or following")),
    }
}

/// The organizer's event, checked for revision; `not_found` for anyone else (no existence oracle).
fn organizer_ev(
    c: &Connection,
    vault: &crate::vault::Vault,
    id: &str,
    me: &str,
    expected: Option<i64>,
) -> GResult<Ev> {
    if !valid_event_id(id) {
        return Err(not_found());
    }
    let ev = load_ev(c, vault, id)?.ok_or_else(not_found)?;
    match access(c, &ev, me)? {
        Some(Access::Organizer) => {}
        Some(Access::Attendee(_)) => {
            return Err(GErr::forbidden(
                "forbidden",
                "only the organizer can change this event",
            ))
        }
        None => return Err(not_found()),
    }
    if ev.status == "cancelled" {
        return Err(GErr::conflict(
            "event_cancelled",
            "this event was cancelled",
        ));
    }
    let expected =
        expected.ok_or_else(|| GErr::bad("bad_request", "expected_revision is required"))?;
    if expected != ev.revision {
        return Err(conflict(ev.revision));
    }
    Ok(ev)
}

/// The occurrence a scoped change names.
fn scoped_occ(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    occurrence_id: &Option<String>,
) -> GResult<Occ> {
    let id = occurrence_id
        .as_deref()
        .ok_or_else(|| GErr::bad("bad_request", "occurrence_id is required for this scope"))?;
    let (eid, key) = parse_occurrence_id(id).ok_or_else(not_found)?;
    if eid != ev.id {
        return Err(not_found());
    }
    occurrence_by_key(c, vault, ev, &key)?.ok_or_else(not_found)
}

/// Notify everyone in the audience of a change: account events to handle attendees (not
/// suppressed), the organizer's own devices, and queued updates for contact guests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn announce(
    c: &Connection,
    ev: &Ev,
    kind: &str,
    occurrence: Option<&str>,
    external_workflow: Option<&str>,
    account: Option<&str>,
    fx: &mut Effects,
    now: u64,
) -> GResult<()> {
    let data = json!({ "event_id": ev.id, "revision": ev.revision, "occurrence_id": occurrence });
    fx.event(c, &ev.organizer, kind, data.clone(), now)?;
    for a in live_attendees(c, &ev.id)? {
        if a.suppressed {
            continue;
        }
        if let Some(occ) = occurrence {
            if !standing(c, &a, Some(occ))?.invited {
                continue;
            }
        } else if a.base != "invited" {
            // One-occurrence invitees hear about series changes only through their occurrence.
            let only: Option<String> = c
                .query_row(
                    "SELECT occurrence_id FROM event_occurrence_attendees
                      WHERE attendee_id = ?1 AND participation = 'invited' LIMIT 1",
                    params![a.id],
                    |r| r.get(0),
                )
                .optional()?;
            if only.is_none() {
                continue;
            }
        }
        match a.kind.as_str() {
            "handle" => {
                if let Some(addr) = &a.address {
                    fx.event(c, addr, kind, data.clone(), now)?;
                }
            }
            _ => {
                if let Some(w) = external_workflow {
                    super::worker::queue(
                        c, ev, &a.id, w, occurrence, a.revision, now, account, now,
                    )?;
                    fx.nudge = true;
                }
            }
        }
    }
    Ok(())
}

/// `PATCH /v1/calendar/events/{id}`
pub(crate) async fn patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = patch_inner(&state, &headers, q.identity.as_deref(), id, body).await;
    finish(&state, r)
}

async fn patch_inner(
    state: &AppState,
    headers: &HeaderMap,
    identity: Option<&str>,
    id: String,
    body: Result<Json<Value>, JsonRejection>,
) -> GResult<(u16, Value, Effects)> {
    let ctx = ctx(state, headers, identity).await?;
    let (input, _): (EventIn, String) = parse(body)?;
    if input.attendees.is_some() {
        return Err(GErr::bad(
            "bad_request",
            "change attendees with POST …/attendees",
        ));
    }
    let scope = scope_of(&input.scope)?;
    let human_ok = ctx.human_with_verified_address();
    let vault = state.vault.clone();
    let inbox = state.calendar.inbox_url.clone();
    let me = ctx.address();
    let account = ctx.account();
    let now = now_unix();
    state
        .store
        .group_tx(
            move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let ev = organizer_ev(tx, &vault, &id, &me, input.expected_revision)?;
                // Changes that send messages to contact addresses need a person (SDS §14B.6).
                let scheduling = input.touches_time()
                    || input.title.is_some()
                    || input.description.is_some()
                    || input.location.is_some()
                    || input.rrule.is_some()
                    || input.exdates.is_some();
                if scheduling
                    && !human_ok
                    && live_attendees(tx, &ev.id)?.iter().any(|a| a.kind == "external" && !a.suppressed)
                {
                    return Err(GErr::forbidden(
                        "human_required",
                        "changes that notify contact-address guests need you signed in yourself, with a verified sign-in address",
                    ));
                }
                let mut fx = Effects::default();
                match scope {
                    "this" => patch_this(
                        tx,
                        &vault,
                        ev,
                        &input,
                        account.as_deref(),
                        &mut fx,
                        now,
                        &me,
                        &inbox,
                    ),
                    "following" => patch_following(
                        tx,
                        &vault,
                        ev,
                        &input,
                        account.as_deref(),
                        &mut fx,
                        now,
                        &me,
                        &inbox,
                    ),
                    _ => patch_all(
                        tx,
                        &vault,
                        ev,
                        &input,
                        account.as_deref(),
                        &mut fx,
                        now,
                        &me,
                        &inbox,
                    ),
                }
            },
        )
        .await
}

/// Apply series-level fields to a copy of `ev`; returns whether scheduling changed (SEQUENCE).
fn apply_series_fields(
    c: &Connection,
    ev: &mut Ev,
    input: &EventIn,
    me: &str,
    now: u64,
) -> GResult<bool> {
    let before = (
        Timing::of(ev),
        ev.rrule.clone(),
        ev.exdates.clone(),
        ev.title.clone(),
        ev.description.clone(),
        ev.location.clone(),
        ev.status.clone(),
    );
    if input.touches_time() {
        let t = timing_from(input, Some(&Timing::of(ev)))?;
        apply_timing(ev, &t);
    }
    text_fields(ev, input)?;
    rule_fields(ev, input)?;
    settle_first(ev);
    if let Some(cal) = &input.calendar_id {
        if !own_calendar(c, me, cal)? {
            return Err(GErr::new(
                StatusCode::NOT_FOUND,
                "calendar_unavailable",
                "no such calendar",
            ));
        }
        ev.calendar_id = cal.clone();
    }
    if let Some(m) = &input.meeting {
        if m.enabled {
            if ev.all_day {
                return Err(GErr::bad(
                    "bad_request",
                    "an all-day event cannot be a meeting",
                ));
            }
            let (mut opts, transport) = meeting_opts(m, ev.meeting.as_ref())?;
            if let Some(names) = &m.admitters {
                opts.admitters = resolve_admitters(c, &ev.id, names)?;
            }
            let non_optional = live_attendees(c, &ev.id)?
                .iter()
                .filter(|a| !a.optional && a.base == "invited")
                .count();
            capacity_check(&transport, limit_for(&transport), non_optional)?;
            if ev.meeting_id.is_none() {
                let (mid, code) = reserve_code(c, me, now)?;
                ev.meeting_id = Some(mid);
                ev.meeting_code = Some(code);
            }
            if ev.meeting.as_ref() != Some(&opts) || ev.transport != transport {
                ev.policy_version += 1;
            }
            ev.meeting = Some(opts);
            ev.participant_limit = limit_for(&transport);
            ev.transport = transport;
        } else if ev.meeting_id.is_some() {
            ev.meeting_id = None;
            ev.meeting_code = None;
            ev.meeting = None;
            ev.policy_version += 1;
        }
    }
    if ev.all_day && ev.meeting_id.is_some() {
        return Err(GErr::bad(
            "bad_request",
            "an all-day event cannot be a meeting",
        ));
    }
    let after = (
        Timing::of(ev),
        ev.rrule.clone(),
        ev.exdates.clone(),
        ev.title.clone(),
        ev.description.clone(),
        ev.location.clone(),
        ev.status.clone(),
    );
    let scheduling = !before.0.same_as(&after.0)
        || before.1 != after.1
        || before.2 != after.2
        || before.3 != after.3
        || before.4 != after.4
        || before.5 != after.5
        || before.6 != after.6;
    Ok(scheduling)
}

/// Retire a reservation nothing uses any more (its code is never handed out again).
pub(crate) fn retire_if_unused(c: &Connection, meeting_id: &str, now: u64) -> GResult<()> {
    c.execute(
        "UPDATE meeting_reservations SET retired_at = ?2
          WHERE id = ?1 AND retired_at IS NULL
            AND NOT EXISTS (SELECT 1 FROM events WHERE meeting_id = ?1 AND status = 'confirmed')",
        params![meeting_id, now as i64],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn patch_all(
    tx: &Connection,
    vault: &crate::vault::Vault,
    ev: Ev,
    input: &EventIn,
    account: Option<&str>,
    fx: &mut Effects,
    now: u64,
    me: &str,
    inbox: &str,
) -> GResult<(u16, Value, Effects)> {
    let old_meeting = ev.meeting_id.clone();
    let mut next = ev.clone();
    let scheduling = apply_series_fields(tx, &mut next, input, me, now)?;
    next.revision += 1;
    if scheduling {
        next.sequence += 1;
    }
    // Exceptions whose key the new series no longer produces are dropped (past ones stay).
    let mut dropped = 0;
    let series = next.series();
    for (key, ov) in load_overrides(tx, vault, &next.id)? {
        let past = ov.key.as_str() < key_now(&next, now).as_str();
        if !past && !series.generates(&key) {
            tx.execute(
                "DELETE FROM event_overrides WHERE occurrence_id = ?1",
                params![ov.occurrence_id],
            )?;
            tx.execute(
                "UPDATE event_occurrences SET status = 'cancelled' WHERE id = ?1",
                params![ov.occurrence_id],
            )?;
            dropped += 1;
        }
    }
    write_ev(tx, vault, &next, false, now)?;
    if let Some(m) = old_meeting.filter(|m| next.meeting_id.as_ref() != Some(m)) {
        retire_if_unused(tx, &m, now)?;
    }
    if let Some(list) = &input.reminders {
        set_reminders(tx, &next.id, Some(me), None, list, false)?;
    }
    announce(
        tx,
        &next,
        "event.updated",
        None,
        scheduling.then_some("update"),
        account,
        fx,
        now,
    )?;
    let next = load_ev(tx, vault, &next.id)?.ok_or_else(not_found)?;
    let view = event_view(tx, vault, &next, me, &Access::Organizer, inbox)?;
    Ok((
        200,
        json!({ "event": view, "dropped": { "exceptions": dropped } }),
        std::mem::take(fx),
    ))
}

/// A recurrence key at "now", for deciding which exceptions are in the past.
fn key_now(ev: &Ev, now: u64) -> String {
    if ev.all_day {
        time::key_date(time::to_local(now as i64, Tz::UTC).date())
    } else {
        time::key_timed(time::to_local(now as i64, ev.tz()))
    }
}

#[allow(clippy::too_many_arguments)]
fn patch_this(
    tx: &Connection,
    vault: &crate::vault::Vault,
    mut ev: Ev,
    input: &EventIn,
    account: Option<&str>,
    fx: &mut Effects,
    now: u64,
    me: &str,
    inbox: &str,
) -> GResult<(u16, Value, Effects)> {
    if !ev.recurring() {
        return patch_all(tx, vault, ev, input, account, fx, now, me, inbox);
    }
    if input.rrule.is_some()
        || input.exdates.is_some()
        || input.meeting.is_some()
        || input.calendar_id.is_some()
    {
        return Err(GErr::bad(
            "bad_request",
            "a single occurrence can change its time, title, description and location only",
        ));
    }
    let occ = scoped_occ(tx, vault, &ev, &input.occurrence_id)?;
    let mut overrides = load_overrides(tx, vault, &ev.id)?;
    let mut ov = overrides.remove(&occ.key).unwrap_or_else(|| Override {
        occurrence_id: occ.id.clone(),
        key: occ.key.clone(),
        ..Override::default()
    });
    if input.touches_time() {
        if input.all_day.is_some_and(|a| a != ev.all_day) {
            return Err(GErr::bad(
                "bad_request",
                "one occurrence cannot switch between all-day and timed",
            ));
        }
        let base = Timing {
            all_day: ev.all_day,
            start_date: Some(occ.start_date()),
            end_date: Some(occ.end_date()),
            local_start: Some(occ.local_start),
            local_end: Some(occ.local_end),
            tz: Some(
                occ.tzid
                    .as_deref()
                    .and_then(|t| time::parse_tz(t).ok())
                    .unwrap_or_else(|| ev.tz()),
            ),
            starts_at: Some(occ.starts_at),
            ends_at: Some(occ.ends_at),
        };
        let t = timing_from(input, Some(&base))?;
        if ev.all_day {
            ov.start_date = t.start_date;
            ov.end_date = t.end_date;
        } else {
            ov.local_start = t.local_start;
            ov.local_end = t.local_end;
            ov.tzid = t.tz.map(|z| z.name().to_string());
        }
    }
    if let Some(t) = &input.title {
        let t = clean_text(t, MAX_TITLE, false);
        if t.is_empty() {
            return Err(GErr::bad("bad_request", "title is required"));
        }
        ov.title = Some(t);
    }
    if let Some(d) = &input.description {
        ov.description = Some(clean_text(d, MAX_DESCRIPTION, true));
    }
    if let Some(l) = &input.location {
        ov.location = Some(clean_text(l, MAX_LOCATION, false));
    }
    ev.revision += 1;
    ev.sequence += 1;
    ov.sequence = ev.sequence;
    write_override(tx, vault, &ev, &ov)?;
    write_ev(tx, vault, &ev, false, now)?;
    let inst_start = occ.local_start; // unused when moved; recomputed below
    let _ = inst_start;
    let fresh = occurrence_by_key(tx, vault, &ev, &occ.key)?.ok_or_else(not_found)?;
    ensure_occurrence(tx, &ev, &fresh)?;
    announce(
        tx,
        &ev,
        "event.updated",
        Some(&fresh.id),
        Some("update"),
        account,
        fx,
        now,
    )?;
    let ev = load_ev(tx, vault, &ev.id)?.ok_or_else(not_found)?;
    let view = event_view(tx, vault, &ev, me, &Access::Organizer, inbox)?;
    Ok((
        200,
        json!({ "event": view, "occurrence": occ_json(&fresh) }),
        std::mem::take(fx),
    ))
}

pub(crate) fn write_override(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    ov: &Override,
) -> GResult<()> {
    // The occurrence row must exist first (foreign key and status).
    let series = ev.series();
    let start = if ev.all_day {
        time::parse_key_date(&ov.key).map(time::midnight)
    } else {
        time::parse_key_timed(&ov.key)
    }
    .ok_or_else(not_found)?;
    let occ = effective(ev, &series.instance_at(start), Some(ov));
    ensure_occurrence(c, ev, &occ)?;
    let title = ov
        .title
        .as_ref()
        .map(|t| seal_v(vault, &format!("{}:title", ov.occurrence_id), t))
        .transpose()?;
    let details = if ov.description.is_some() || ov.location.is_some() {
        Some(seal_v(
            vault,
            &format!("{}:details", ov.occurrence_id),
            &json!({ "description": ov.description, "location": ov.location }).to_string(),
        )?)
    } else {
        None
    };
    c.execute(
        "INSERT INTO event_overrides (occurrence_id, local_start, local_end, tzid, start_date,
             end_date_exclusive, title_sealed, details_sealed, status, revision, sequence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(occurrence_id) DO UPDATE SET local_start = excluded.local_start,
            local_end = excluded.local_end, tzid = excluded.tzid, start_date = excluded.start_date,
            end_date_exclusive = excluded.end_date_exclusive, title_sealed = excluded.title_sealed,
            details_sealed = excluded.details_sealed, status = excluded.status,
            revision = excluded.revision, sequence = excluded.sequence",
        params![
            ov.occurrence_id,
            ov.local_start.map(time::fmt_local),
            ov.local_end.map(time::fmt_local),
            ov.tzid,
            ov.start_date.map(time::fmt_date),
            ov.end_date.map(time::fmt_date),
            title,
            details,
            ov.status,
            ev.revision,
            ov.sequence
        ],
    )?;
    Ok(())
}

/// End the series before `occ` (for "following" changes and cancellations). Returns the number
/// of occurrences kept, or 0 when `occ` is the first (the caller then treats it as "all").
fn truncate_before(ev: &mut Ev, occ: &Occ) -> GResult<usize> {
    let series = ev.series();
    let start = if ev.all_day {
        time::parse_key_date(&occ.key).map(time::midnight)
    } else {
        time::parse_key_timed(&occ.key)
    }
    .ok_or_else(not_found)?;
    let (kept, last) = series
        .count_before(start, time::MAX_COUNT as usize * 20)
        .ok_or_else(|| GErr::bad("expansion_limit", "this series is too long to split here"))?;
    let Some(last) = last else { return Ok(0) };
    let mut rule = series.rule.clone().ok_or_else(not_found)?;
    rule.set("COUNT", None);
    let until = if ev.all_day {
        time::key_date(last.start_date())
    } else {
        format!(
            "{}Z",
            time::key_timed(time::to_local(last.starts_at, Tz::UTC))
        )
    };
    rule.set("UNTIL", Some(until));
    ev.rrule = Some(rule.body());
    Ok(kept)
}

#[allow(clippy::too_many_arguments)]
fn patch_following(
    tx: &Connection,
    vault: &crate::vault::Vault,
    ev: Ev,
    input: &EventIn,
    account: Option<&str>,
    fx: &mut Effects,
    now: u64,
    me: &str,
    inbox: &str,
) -> GResult<(u16, Value, Effects)> {
    if !ev.recurring() {
        return patch_all(tx, vault, ev, input, account, fx, now, me, inbox);
    }
    let occ = scoped_occ(tx, vault, &ev, &input.occurrence_id)?;
    let original_rule = ev.series().rule.clone().ok_or_else(not_found)?;
    let mut old = ev.clone();
    let kept = truncate_before(&mut old, &occ)?;
    if kept == 0 {
        return patch_all(tx, vault, ev, input, account, fx, now, me, inbox);
    }
    old.revision += 1;
    old.sequence += 1;

    // The new series starts at the chosen occurrence, with the remaining COUNT (if any).
    let mut new = ev.clone();
    new.id = new_id("evt");
    new.revision = 1;
    new.sequence = 0;
    new.split_from = Some(ev.id.clone());
    if ev.all_day {
        new.start_date = Some(occ.start_date());
        new.end_date = Some(
            occ.start_date()
                + (ev.end_date.unwrap_or_default() - ev.start_date.unwrap_or_default()),
        );
    } else {
        let start = time::parse_key_timed(&occ.key).ok_or_else(not_found)?;
        let len = ev.local_end.unwrap_or_default() - ev.local_start.unwrap_or_default();
        new.local_start = Some(start);
        new.local_end = Some(start + len);
        let s = time::resolve_rfc(start, ev.tz());
        new.starts_at = Some(s);
        new.ends_at = Some(time::resolve_rfc(start + len, ev.tz()).max(s + 60));
    }
    let mut rule = original_rule.clone();
    if let Some(count) = rule.count() {
        rule.set(
            "COUNT",
            Some((count as usize).saturating_sub(kept).max(1).to_string()),
        );
    }
    new.rrule = Some(rule.body());
    new.exdates.retain(|k| k.as_str() >= occ.key.as_str());
    let unchanged_base = Timing::of(&new);
    let _scheduling = apply_series_fields(tx, &mut new, input, me, now)?;
    let same_time = Timing::of(&new).same_as(&unchanged_base)
        && new.rrule.as_deref() == Some(rule.body().as_str());
    old.exdates.retain(|k| k.as_str() < occ.key.as_str());
    write_ev(tx, vault, &old, false, now)?;
    write_ev(tx, vault, &new, true, now)?;

    // Attendees carry over with fresh ids; their answers too when the time did not change.
    let mut map: HashMap<String, String> = HashMap::new();
    let mut responses_carried = 0;
    let mut responses_dropped = 0;
    for a in live_attendees(tx, &old.id)? {
        let nid = new_id("att");
        let keep_rsvp = same_time && a.rsvp != "needs_action";
        if a.rsvp != "needs_action" {
            if keep_rsvp {
                responses_carried += 1;
            } else {
                responses_dropped += 1;
            }
        }
        tx.execute(
            "INSERT INTO event_attendees (id, event_id, kind, address, contact_hash, contact_sealed,
                 display_name, role, optional, base_participation, rsvp, rsvp_at, rsvp_proof,
                 calendar_id, filtered, suppressed, can_admit, added_at)
             SELECT ?1, ?2, kind, address, contact_hash, NULL, display_name, role, optional,
                    base_participation, ?3, ?4, ?5, calendar_id, filtered, suppressed, can_admit, ?6
               FROM event_attendees WHERE id = ?7",
            params![
                nid,
                new.id,
                if keep_rsvp { a.rsvp.as_str() } else { "needs_action" },
                if keep_rsvp { a.rsvp_at } else { None },
                if keep_rsvp { a.rsvp_proof.clone() } else { None },
                now as i64,
                a.id
            ],
        )?;
        if let Some(sealed) = &a.contact_sealed {
            let plain = open(vault, &format!("{}:contact", a.id), sealed);
            let resealed = seal_v(vault, &format!("{nid}:contact"), &plain)?;
            tx.execute(
                "UPDATE event_attendees SET contact_sealed = ?2 WHERE id = ?1",
                params![nid, resealed],
            )?;
        }
        tx.execute(
            "INSERT INTO event_reminders (id, event_id, attendee_id, minutes_before, channel)
             SELECT ?1 || '_' || id, ?2, ?3, minutes_before, channel FROM event_reminders
              WHERE attendee_id = ?4 AND enabled = 1",
            params![new_id("rem"), new.id, nid, a.id],
        )?;
        map.insert(a.id.clone(), nid);
    }
    tx.execute(
        "INSERT INTO event_reminders (id, event_id, owner_identity, minutes_before, channel)
         SELECT ?1 || '_' || id, ?2, owner_identity, minutes_before, channel FROM event_reminders
          WHERE event_id = ?3 AND owner_identity IS NOT NULL AND enabled = 1",
        params![new_id("rem"), new.id, old.id],
    )?;
    if let Some(m) = new.meeting.as_mut() {
        m.admitters = m
            .admitters
            .iter()
            .filter_map(|a| map.get(a).cloned())
            .collect();
        write_ev(tx, vault, &new, false, now)?;
    }

    // Exceptions and per-occurrence answers at or after the split move when the new series still
    // produces the same occurrence; otherwise they are dropped and counted.
    let new_series = new.series();
    let (mut carried, mut dropped) = (0, 0);
    for (key, ov) in load_overrides(tx, vault, &old.id)? {
        if key.as_str() < occ.key.as_str() {
            continue;
        }
        if same_time && new_series.generates(&key) {
            let mut moved = ov.clone();
            moved.occurrence_id = occurrence_id(&new.id, &key);
            write_override(tx, vault, &new, &moved)?;
            tx.execute(
                "INSERT INTO event_occurrence_attendees (occurrence_id, attendee_id, participation, rsvp, role, rsvp_at)
                 SELECT ?1, x.new_id, o.participation, o.rsvp, o.role, o.rsvp_at
                   FROM event_occurrence_attendees o
                   JOIN (SELECT value AS pair FROM json_each(?3)) j
                   JOIN (SELECT json_extract(j2.value, '$[0]') AS old_id, json_extract(j2.value, '$[1]') AS new_id
                           FROM json_each(?3) j2) x ON x.old_id = o.attendee_id
                  WHERE o.occurrence_id = ?2
                  GROUP BY x.new_id",
                params![
                    moved.occurrence_id,
                    ov.occurrence_id,
                    serde_json::to_string(&map.iter().map(|(a, b)| vec![a.clone(), b.clone()]).collect::<Vec<_>>())
                        .expect("serializes")
                ],
            )?;
            carried += 1;
        } else {
            dropped += 1;
        }
        tx.execute(
            "DELETE FROM event_occurrence_attendees WHERE occurrence_id = ?1",
            params![ov.occurrence_id],
        )?;
        tx.execute(
            "DELETE FROM event_overrides WHERE occurrence_id = ?1",
            params![ov.occurrence_id],
        )?;
    }

    announce(
        tx,
        &old,
        "event.updated",
        None,
        Some("update"),
        account,
        fx,
        now,
    )?;
    // The new series is a new invitation (new UID) to everyone carried over.
    fx.event(
        tx,
        &new.organizer,
        "event.updated",
        json!({ "event_id": new.id, "revision": new.revision }),
        now,
    )?;
    for a in live_attendees(tx, &new.id)? {
        if a.suppressed {
            continue;
        }
        if a.kind == "handle" {
            if let Some(addr) = &a.address {
                fx.event(tx, addr, "event.invite", json!({ "event_id": new.id, "revision": new.revision,
                                                           "filtered": a.filtered, "split_from": old.id }), now)?;
            }
        } else {
            super::worker::queue(
                tx, &new, &a.id, "invite", None, a.revision, now, account, now,
            )?;
            fx.nudge = true;
        }
    }
    let new = load_ev(tx, vault, &new.id)?.ok_or_else(not_found)?;
    let view = event_view(tx, vault, &new, me, &Access::Organizer, inbox)?;
    Ok((
        200,
        json!({
            "event": view,
            "split_from": { "event_id": old.id, "revision": old.revision },
            "carried": { "exceptions": carried, "responses": responses_carried },
            "dropped": { "exceptions": dropped, "responses": responses_dropped },
        }),
        std::mem::take(fx),
    ))
}

// ---- cancel ------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct CancelIn {
    #[serde(default)]
    expected_revision: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// `POST /v1/calendar/events/{id}/cancel`
pub(crate) async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (CancelIn, String) = parse(body)?;
        let scope = scope_of(&input.scope)?;
        let vault = state.vault.clone();
        let me = ctx.address();
        let account = ctx.account();
        let now = now_unix();
        let message = input.message.as_deref().map(|m| clean_text(m, 1000, true)).filter(|m| !m.is_empty());
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let mut ev = organizer_ev(tx, &vault, &id, &me, input.expected_revision)?;
                let mut fx = Effects::default();
                let mut scope = scope;
                if scope != "all" && !ev.recurring() {
                    scope = "all";
                }
                let mut occ_id = None;
                match scope {
                    "this" => {
                        let occ = scoped_occ(tx, &vault, &ev, &input.occurrence_id)?;
                        let mut ov = load_overrides(tx, &vault, &ev.id)?
                            .remove(&occ.key)
                            .unwrap_or_else(|| Override {
                                occurrence_id: occ.id.clone(),
                                key: occ.key.clone(),
                                ..Override::default()
                            });
                        ev.revision += 1;
                        ev.sequence += 1;
                        ov.status = Some("cancelled".into());
                        ov.sequence = ev.sequence;
                        write_override(tx, &vault, &ev, &ov)?;
                        write_ev(tx, &vault, &ev, false, now)?;
                        occ_id = Some(occ.id);
                    }
                    "following" => {
                        let occ = scoped_occ(tx, &vault, &ev, &input.occurrence_id)?;
                        let kept = truncate_before(&mut ev, &occ)?;
                        if kept == 0 {
                            ev.status = "cancelled".into();
                        }
                        ev.exdates.retain(|k| k.as_str() < occ.key.as_str());
                        ev.revision += 1;
                        ev.sequence += 1;
                        write_ev(tx, &vault, &ev, false, now)?;
                    }
                    _ => {
                        ev.status = "cancelled".into();
                        ev.revision += 1;
                        ev.sequence += 1;
                        write_ev(tx, &vault, &ev, false, now)?;
                        if let Some(m) = &ev.meeting_id {
                            retire_if_unused(tx, m, now)?;
                        }
                    }
                }
                if let Some(m) = &message {
                    // Kept with the cancellation notices only (sealed), never in query logs.
                    let sealed = seal_v(&vault, &format!("{}:cancel-note:{}", ev.id, ev.revision), m)?;
                    tx.execute(
                        "INSERT OR REPLACE INTO calendar_idempotency (scope, action, request_key, body_hash, status, response, created_at)
                         VALUES (?1, 'cancel_note', ?2, '', 0, ?3, ?4)",
                        params![ev.id, ev.revision.to_string(), crate::b64_encode(&sealed), now as i64],
                    )?;
                }
                let kind = if scope == "following" && ev.status == "confirmed" {
                    "event.updated"
                } else {
                    "event.cancelled"
                };
                let workflow = if kind == "event.updated" { "update" } else { "cancel" };
                announce(tx, &ev, kind, occ_id.as_deref(), Some(workflow), account.as_deref(), &mut fx, now)?;
                Ok((200, json!({ "event_id": ev.id, "revision": ev.revision, "status": ev.status,
                                  "occurrence_id": occ_id }), fx))
            })
            .await
    }
    .await;
    finish(&state, r)
}

// ---- attendees ---------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct AttendeesIn {
    #[serde(default)]
    expected_revision: Option<i64>,
    #[serde(default)]
    add: Vec<AttendeeIn>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
}

/// `POST /v1/calendar/events/{id}/attendees`
pub(crate) async fn attendees(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (AttendeesIn, String) = parse(body)?;
        let scope = match input.scope.as_deref().unwrap_or("all") {
            "all" => "all",
            "this" => "this",
            _ => return Err(GErr::bad("bad_request", "scope is all or this")),
        };
        if input.add.len() > MAX_HANDLE_ATTENDEES + MAX_EXTERNAL_ATTENDEES || input.remove.len() > 1000 {
            return Err(GErr::bad("attendee_limit", "too many changes at once"));
        }
        let judged = judge(&state, &ctx.me, &input.add).await?;
        let externals = judged.iter().filter(|j| matches!(j.who, Who::External { .. })).count();
        let score = crate::sender_score(&state, &ctx.me).await;
        external_gate(&ctx, score, externals)?;
        let vault = state.vault.clone();
        let inbox = state.calendar.inbox_url.clone();
        let me = ctx.address();
        let account = ctx.account();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let mut ev = organizer_ev(tx, &vault, &id, &me, input.expected_revision)?;
                let mut fx = Effects::default();
                let occ = if scope == "this" {
                    Some(scoped_occ(tx, &vault, &ev, &input.occurrence_id)?)
                } else {
                    None
                };
                ev.revision += 1;
                let live = live_attendees(tx, &ev.id)?;
                // Removals first.
                for rid in &input.remove {
                    let Some(a) = live.iter().find(|a| &a.id == rid) else {
                        return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no such attendee"));
                    };
                    match &occ {
                        None => {
                            tx.execute(
                                "UPDATE event_attendees SET removed_at = ?2, grant_version = grant_version + 1,
                                        revision = revision + 1 WHERE id = ?1",
                                params![a.id, now as i64],
                            )?;
                            revoke_grants(tx, &a.id, now)?;
                            if !a.suppressed {
                                match a.kind.as_str() {
                                    "handle" => {
                                        if let Some(addr) = &a.address {
                                            fx.event(tx, addr, "event.cancelled",
                                                     json!({ "event_id": ev.id, "revision": ev.revision, "removed": true }), now)?;
                                        }
                                    }
                                    _ => {
                                        super::worker::queue(tx, &ev, &a.id, "cancel", None, a.revision + 1, now, account.as_deref(), now)?;
                                        fx.nudge = true;
                                    }
                                }
                            }
                        }
                        Some(o) => {
                            ensure_occurrence(tx, &ev, o)?;
                            tx.execute(
                                "INSERT INTO event_occurrence_attendees (occurrence_id, attendee_id, participation)
                                 VALUES (?1, ?2, 'removed')
                                 ON CONFLICT(occurrence_id, attendee_id) DO UPDATE SET participation = 'removed',
                                    revision = revision + 1",
                                params![o.id, a.id],
                            )?;
                            if !a.suppressed {
                                if a.kind == "handle" {
                                    if let Some(addr) = &a.address {
                                        fx.event(tx, addr, "event.cancelled",
                                                 json!({ "event_id": ev.id, "revision": ev.revision, "occurrence_id": o.id }), now)?;
                                    }
                                } else {
                                    super::worker::queue(tx, &ev, &a.id, "cancel", Some(&o.id), a.revision + 1, now, account.as_deref(), now)?;
                                    fx.nudge = true;
                                }
                            }
                        }
                    }
                }
                // Additions.
                if externals > 0
                    && external_invites_today(tx, account.as_deref(), now)? + externals as i64 > EXTERNAL_INVITES_PER_DAY
                {
                    return Err(GErr::new(StatusCode::TOO_MANY_REQUESTS, "invite_limit",
                        format!("at most {EXTERNAL_INVITES_PER_DAY} invitations to contact addresses a day")));
                }
                let live = live_attendees(tx, &ev.id)?;
                let mut added = Vec::new();
                for j in &judged {
                    let existing = live.iter().find(|a| match &j.who {
                        Who::Handle { address, .. } => a.address.as_deref() == Some(address.as_str()),
                        Who::External { hash, .. } => a.contact_hash.as_deref() == Some(hash.as_slice()),
                    });
                    match (existing, &occ) {
                        (Some(a), None) if a.base == "excluded" => {
                            tx.execute(
                                "UPDATE event_attendees SET base_participation = 'invited', revision = revision + 1 WHERE id = ?1",
                                params![a.id],
                            )?;
                            added.push(a.id.clone());
                        }
                        (Some(a), Some(o)) => {
                            ensure_occurrence(tx, &ev, o)?;
                            tx.execute(
                                "INSERT INTO event_occurrence_attendees (occurrence_id, attendee_id, participation)
                                 VALUES (?1, ?2, 'invited')
                                 ON CONFLICT(occurrence_id, attendee_id) DO UPDATE SET participation = 'invited',
                                    revision = revision + 1",
                                params![o.id, a.id],
                            )?;
                            added.push(a.id.clone());
                        }
                        (Some(_), None) => {}
                        (None, o) => {
                            added.push(insert_attendee(tx, &vault, &ev, j, o.as_ref(), account.as_deref(), &mut fx, now)?);
                        }
                    }
                }
                let live = live_attendees(tx, &ev.id)?;
                let handles = live.iter().filter(|a| a.kind == "handle").count();
                let externals_now = live.len() - handles;
                if handles > MAX_HANDLE_ATTENDEES || externals_now > MAX_EXTERNAL_ATTENDEES {
                    return Err(GErr::bad("attendee_limit", format!(
                        "at most {MAX_HANDLE_ATTENDEES} people with handles and {MAX_EXTERNAL_ATTENDEES} contact addresses per event")));
                }
                if ev.meeting.is_some() {
                    let non_optional = live.iter().filter(|a| !a.optional && a.base == "invited").count();
                    capacity_check(&ev.transport, ev.participant_limit, non_optional)?;
                }
                write_ev(tx, &vault, &ev, false, now)?;
                fx.event(tx, &me, "event.updated", json!({ "event_id": ev.id, "revision": ev.revision }), now)?;
                let ev = load_ev(tx, &vault, &ev.id)?.ok_or_else(not_found)?;
                let view = event_view(tx, &vault, &ev, &me, &Access::Organizer, &inbox)?;
                Ok((200, json!({ "event": view, "added": added }), fx))
            })
            .await
    }
    .await;
    finish(&state, r)
}

/// Revoke an attendee's outstanding credentials (links, guest sessions, join tickets).
pub(crate) fn revoke_grants(c: &Connection, attendee_id: &str, now: u64) -> GResult<()> {
    c.execute(
        "UPDATE event_attendee_tokens SET revoked_at = ?2
          WHERE attendee_id = ?1 AND purpose = 'invite_manage' AND revoked_at IS NULL",
        params![attendee_id, now as i64],
    )?;
    c.execute(
        "UPDATE event_guest_sessions SET revoked_at = ?2 WHERE attendee_id = ?1 AND revoked_at IS NULL",
        params![attendee_id, now as i64],
    )?;
    c.execute(
        "UPDATE event_join_tickets SET redeemed_at = COALESCE(redeemed_at, ?2) WHERE attendee_id = ?1",
        params![attendee_id, now as i64],
    )?;
    Ok(())
}

// ---- RSVP, reminders, placement ----------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct RsvpIn {
    pub status: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub occurrence_id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
}

/// Record an answer for `att` (member or guest). Shared with the guest route.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_rsvp(
    tx: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    att: &Att,
    input: &RsvpIn,
    proof: &str,
    fx: &mut Effects,
    now: u64,
) -> GResult<Value> {
    let status = one_of(
        "status",
        &input.status,
        &["accepted", "tentative", "declined"],
    )?;
    let note = input
        .note
        .as_deref()
        .map(|n| clean_text(n, MAX_NOTE, false))
        .filter(|n| !n.is_empty());
    if ev.status != "confirmed" {
        return Err(GErr::conflict(
            "event_cancelled",
            "this event was cancelled",
        ));
    }
    if let Some(expected) = input.expected_revision {
        if expected != att.revision {
            return Err(conflict(att.revision));
        }
    }
    let scope = match input.scope.as_deref().unwrap_or("all") {
        "all" => "all",
        "this" => "this",
        _ => return Err(GErr::bad("bad_request", "scope is all or this")),
    };
    let sealed = note
        .as_ref()
        .map(|n| seal_v(vault, &format!("{}:note", att.id), n))
        .transpose()?;
    let mut occ_id = None;
    if scope == "this" || att.base != "invited" {
        let occ = scoped_occ(tx, vault, ev, &input.occurrence_id)?;
        if !standing(tx, att, Some(&occ.id))?.invited {
            return Err(not_found());
        }
        ensure_occurrence(tx, ev, &occ)?;
        tx.execute(
            "INSERT INTO event_occurrence_attendees (occurrence_id, attendee_id, rsvp, rsvp_note_sealed, rsvp_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(occurrence_id, attendee_id) DO UPDATE SET rsvp = excluded.rsvp,
                rsvp_note_sealed = excluded.rsvp_note_sealed, rsvp_at = excluded.rsvp_at,
                revision = revision + 1",
            params![occ.id, att.id, status, sealed, now as i64],
        )?;
        tx.execute(
            "UPDATE event_attendees SET filtered = 0, rsvp_proof = ?2, revision = revision + 1 WHERE id = ?1",
            params![att.id, proof],
        )?;
        occ_id = Some(occ.id);
    } else {
        // A series answer replaces earlier per-occurrence answers.
        tx.execute(
            "UPDATE event_occurrence_attendees SET rsvp = NULL, rsvp_note_sealed = NULL, rsvp_at = NULL
              WHERE attendee_id = ?1",
            params![att.id],
        )?;
        tx.execute(
            "UPDATE event_attendees SET rsvp = ?2, rsvp_note_sealed = ?3, rsvp_at = ?4, rsvp_proof = ?5,
                    filtered = 0, revision = revision + 1 WHERE id = ?1",
            params![att.id, status, sealed, now as i64, proof],
        )?;
    }
    let revision: i64 = tx.query_row(
        "SELECT revision FROM event_attendees WHERE id = ?1",
        params![att.id],
        |r| r.get(0),
    )?;
    fx.event(
        tx,
        &ev.organizer,
        "event.rsvp",
        json!({ "event_id": ev.id, "attendee_id": att.id, "revision": revision, "occurrence_id": occ_id }),
        now,
    )?;
    if let Some(addr) = &att.address {
        fx.event(
            tx,
            addr,
            "event.updated",
            json!({ "event_id": ev.id, "revision": ev.revision, "occurrence_id": occ_id }),
            now,
        )?;
    }
    Ok(
        json!({ "event_id": ev.id, "attendee_id": att.id, "rsvp": status, "occurrence_id": occ_id,
               "revision": revision, "proof": proof }),
    )
}

/// `POST /v1/calendar/events/{id}/rsvp`
pub(crate) async fn rsvp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (RsvpIn, String) = parse(body)?;
        let vault = state.vault.clone();
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    if !valid_event_id(&id) {
                        return Err(not_found());
                    }
                    let ev = load_ev(tx, &vault, &id)?.ok_or_else(not_found)?;
                    let att = match access(tx, &ev, &me)? {
                        Some(Access::Attendee(a)) => a,
                        Some(Access::Organizer) => {
                            return Err(GErr::bad(
                                "bad_request",
                                "the organizer does not answer their own event",
                            ))
                        }
                        None => return Err(not_found()),
                    };
                    let mut fx = Effects::default();
                    let v = record_rsvp(tx, &vault, &ev, &att, &input, "member", &mut fx, now)?;
                    Ok((200, v, fx))
                },
            )
            .await
    }
    .await;
    finish(&state, r)
}

#[derive(Deserialize, Default)]
pub(crate) struct RemindersIn {
    reminders: Vec<ReminderIn>,
}

/// `PUT /v1/calendar/events/{id}/reminders`
pub(crate) async fn reminders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (RemindersIn, String) = parse(body)?;
        let vault = state.vault.clone();
        let me = ctx.address();
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    if !valid_event_id(&id) {
                        return Err(not_found());
                    }
                    let ev = load_ev(tx, &vault, &id)?.ok_or_else(not_found)?;
                    let out = match access(tx, &ev, &me)?.ok_or_else(not_found)? {
                        Access::Organizer => {
                            set_reminders(tx, &ev.id, Some(&me), None, &input.reminders, false)?;
                            my_reminders(tx, &ev.id, Some(&me), None)?
                        }
                        Access::Attendee(a) => {
                            set_reminders(tx, &ev.id, None, Some(&a.id), &input.reminders, false)?;
                            my_reminders(tx, &ev.id, None, Some(&a.id))?
                        }
                    };
                    Ok((200, json!({ "reminders": out }), Effects::default()))
                },
            )
            .await
    }
    .await;
    finish(&state, r)
}

#[derive(Deserialize, Default)]
pub(crate) struct PlacementIn {
    calendar_id: String,
}

/// `PUT /v1/calendar/events/{id}/placement`
pub(crate) async fn placement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (PlacementIn, String) = parse(body)?;
        let vault = state.vault.clone();
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(
                move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                    if !valid_event_id(&id) {
                        return Err(not_found());
                    }
                    let ev = load_ev(tx, &vault, &id)?.ok_or_else(not_found)?;
                    let a = match access(tx, &ev, &me)?.ok_or_else(not_found)? {
                        Access::Attendee(a) => a,
                        Access::Organizer => {
                            return Err(GErr::bad(
                                "bad_request",
                                "move your own event with PATCH calendar_id",
                            ))
                        }
                    };
                    if !own_calendar(tx, &me, &input.calendar_id)? {
                        return Err(GErr::new(
                            StatusCode::NOT_FOUND,
                            "calendar_unavailable",
                            "no such calendar",
                        ));
                    }
                    tx.execute(
                        "UPDATE event_attendees SET calendar_id = ?2 WHERE id = ?1",
                        params![a.id, input.calendar_id],
                    )?;
                    let mut fx = Effects::default();
                    fx.event(
                        tx,
                        &me,
                        "event.updated",
                        json!({ "event_id": ev.id, "revision": ev.revision }),
                        now,
                    )?;
                    Ok((
                        200,
                        json!({ "event_id": ev.id, "calendar_id": input.calendar_id }),
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
pub(crate) struct LinkedGroupIn {
    #[serde(default)]
    expected_revision: Option<i64>,
    #[serde(default)]
    group_id: Option<String>,
}

/// `PATCH /v1/calendar/events/{id}/linked-group`
pub(crate) async fn linked_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        let (input, _): (LinkedGroupIn, String) = parse(body)?;
        let vault = state.vault.clone();
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                let mut ev = organizer_ev(tx, &vault, &id, &me, input.expected_revision)?;
                if let Some(g) = &input.group_id {
                    let member: Option<String> = tx
                        .query_row(
                            "SELECT m.state FROM group_members m JOIN groups g ON g.id = m.group_id
                              WHERE m.group_id = ?1 AND m.member = ?2 AND g.deleted_at IS NULL",
                            params![g, me],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if member.as_deref() != Some("active") {
                        return Err(GErr::forbidden("not_member", "you are not an active member of that group"));
                    }
                }
                ev.linked_group = input.group_id.clone();
                ev.revision += 1;
                write_ev(tx, &vault, &ev, false, now)?;
                let mut fx = Effects::default();
                announce(tx, &ev, "event.updated", None, None, None, &mut fx, now)?;
                Ok((200, json!({ "event_id": ev.id, "revision": ev.revision, "linked_group": ev.linked_group }), fx))
            })
            .await
    }
    .await;
    finish(&state, r)
}

// ---- invitations and links ---------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct InvitationsQuery {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// `GET /v1/calendar/invitations`
pub(crate) async fn invitations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InvitationsQuery>,
) -> Response {
    let ctx = match ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let filtered = match q.state.as_deref().unwrap_or("pending") {
        "pending" => false,
        "filtered" => true,
        _ => return GErr::bad("bad_request", "state is pending or filtered").into_response(),
    };
    let me = ctx.address();
    let vault = state.vault.clone();
    let now = now_unix() as i64;
    let r = state
        .store
        .group_read(move |c: &Connection| -> GResult<Value> {
            let mut stmt = c.prepare(&format!(
                "SELECT {ATT_COLS_A} FROM event_attendees a JOIN events e ON e.id = a.event_id
                  WHERE a.address = ?1 AND a.kind = 'handle' AND a.removed_at IS NULL
                    AND a.suppressed = 0 AND a.filtered = ?2 AND e.status = 'confirmed'
                    AND (e.span_to IS NULL OR e.span_to > ?3)
                  ORDER BY a.added_at DESC LIMIT 200"
            ))?;
            let atts = stmt
                .query_map(params![me, filtered as i64, now], read_att)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = Vec::new();
            for a in atts {
                if !filtered && a.rsvp != "needs_action" {
                    continue;
                }
                let Some(ev) = load_ev(c, &vault, &a.event_id)? else {
                    continue;
                };
                // The next occurrence they are invited to.
                let occs = occurrences_in(
                    c,
                    &vault,
                    &ev,
                    now,
                    now + time::MAX_WINDOW_SECS,
                    time::MAX_OCCURRENCES,
                )
                .unwrap_or_default();
                let mut next = None;
                for o in occs {
                    if o.status == "confirmed" && standing(c, &a, Some(&o.id))?.invited {
                        next = Some(o);
                        break;
                    }
                }
                let next = next.or_else(|| ev.series().first().map(|i| effective(&ev, &i, None)));
                let Some(o) = next else { continue };
                let mut v = occ_json(&o);
                v["event_id"] = json!(ev.id);
                v["title"] = json!(o.title);
                v["organizer"] = person(c, &ev.organizer)?;
                v["recurring"] = json!(ev.recurring());
                v["meeting"] = json!(ev.meeting_code.is_some());
                v["invited_at"] = json!(a.added_at);
                v["attendee_id"] = json!(a.id);
                out.push(v);
            }
            Ok(json!({ "invitations": out }))
        })
        .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// How long an invitation link lasts: 30 days, but never past the event's end.
pub(crate) fn link_expiry(ev: &Ev, now: u64) -> i64 {
    let cap = now as i64 + 30 * 86_400;
    match ev.span_to {
        Some(end) => cap.min(end + JOIN_AFTER_SECS).max(now as i64 + 3600),
        None => cap,
    }
}

/// Mint a fresh invitation-management token for a contact guest. Returns the bearer value.
pub(crate) fn mint_invite_token(
    c: &Connection,
    ev: &Ev,
    att: &Att,
    now: u64,
) -> GResult<(String, i64)> {
    let token = new_secret();
    let expires = link_expiry(ev, now);
    c.execute(
        "INSERT INTO event_attendee_tokens (id, token_hash, attendee_id, purpose, audience,
             attendee_grant_version, created_at, expires_at)
         VALUES (?1, ?2, ?3, 'invite_manage', 'calendar-guest', ?4, ?5, ?6)",
        params![
            new_id("tok"),
            secret_hash(&token),
            att.id,
            att.grant_version,
            now as i64,
            expires
        ],
    )?;
    Ok((token, expires))
}

/// `POST /v1/calendar/events/{id}/attendees/{attendee_id}/link`
pub(crate) async fn attendee_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, attendee_id)): Path<(String, String)>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let r = async {
        let ctx = ctx(&state, &headers, q.identity.as_deref()).await?;
        if !ctx.human() {
            return Err(GErr::forbidden("human_required", "only the organizer, signed in themselves, can do this"));
        }
        crate::groups::rate_limit(&format!("cal-link:{}", ctx.address()), 30, 3600)?;
        let vault = state.vault.clone();
        let inbox = state.calendar.inbox_url.clone();
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx: &Transaction<'_>| -> GResult<(u16, Value, Effects)> {
                if !valid_event_id(&id) || !valid_attendee_id(&attendee_id) {
                    return Err(not_found());
                }
                let ev = load_ev(tx, &vault, &id)?.ok_or_else(not_found)?;
                if ev.organizer != me {
                    return Err(not_found());
                }
                if ev.status != "confirmed" {
                    return Err(GErr::conflict("event_cancelled", "this event was cancelled"));
                }
                let att = attendee(tx, &attendee_id)?
                    .filter(|a| a.event_id == ev.id && a.kind == "external" && a.removed_at.is_none())
                    .ok_or_else(not_found)?;
                let (token, expires) = mint_invite_token(tx, &ev, &att, now)?;
                Ok((200, json!({ "url": format!("{inbox}/m/{}#t={token}", ev.locator()), "expires_at": expires }),
                    Effects::default()))
            })
            .await
    }
    .await;
    finish(&state, r)
}
