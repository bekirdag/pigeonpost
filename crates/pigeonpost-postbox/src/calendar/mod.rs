//! Calendar and scheduled meetings (SDS §14B). Client contract: `docs/calendar.md`.
//!
//! # Shape
//!
//! Like groups (`crate::groups`), every mutation is one function over one IMMEDIATE SQLite
//! transaction (`Store::group_tx`): authorization, revision checks, occurrence and attendee
//! changes, the delivery outbox and account events commit together, and side effects that leave
//! the process (push, wakeups, the delivery worker) run only after the commit.
//!
//! * `time` — wall times, zones, RFC 5545 recurrence and recurrence keys.
//! * `events` — event and occurrence routes for members.
//! * `calendars` — calendars, settings, feeds, availability, `.ics` import.
//! * `guest` — the routes for contact-address guests (no Pigeonpost account).
//! * `meeting` — `POST /internal/v1/calendar/meeting-access` for the calls service.
//! * `ics` — iCalendar writing (RFC 5545/6047) and bounded, network-free reading.
//! * `mail` — invitation and reminder messages (EN/TR, light and dark) and the saas_be client.
//! * `worker` — the leased delivery outbox, reminders and retention.
//!
//! Calendar data is custodial: titles, details and notes are sealed at rest under the vault and
//! readable by this service. Nothing here may be described to users as end-to-end encrypted.

pub(crate) mod calendars;
pub(crate) mod events;
pub(crate) mod guest;
pub(crate) mod ics;
pub(crate) mod mail;
pub(crate) mod meeting;
pub(crate) mod time;
pub(crate) mod worker;

#[cfg(test)]
mod tests;

use crate::capabilities;
use crate::filtering::{feature_on, mailbox_for};
use crate::groups::{rand_b32, GErr, GResult};
use crate::policy::Action;
use crate::{push, store, AppState, Principal};
use axum::http::{HeaderMap, StatusCode};
use chrono::{Duration, NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

// ---- limits (SDS §14B.6, §14B.14, D-31) ---------------------------------------------------------

/// Handle attendees per event.
pub(crate) const MAX_HANDLE_ATTENDEES: usize = 500;
/// Contact-address guests per event.
pub(crate) const MAX_EXTERNAL_ATTENDEES: usize = 50;
/// New contact-address invitations per account per day.
pub(crate) const EXTERNAL_INVITES_PER_DAY: i64 = 200;
/// Events created per mailbox per day.
const EVENTS_PER_DAY: usize = 500;
/// Calendars per mailbox.
pub(crate) const MAX_CALENDARS: i64 = 20;
/// Reminders per person per event.
pub(crate) const MAX_REMINDERS: usize = 5;
/// The mesh profile's size (organizer and guests counted) and the large-call profile's (SFU).
pub(crate) const MESH_LIMIT: i64 = 6;
pub(crate) const SFU_LIMIT: i64 = 30;
/// Join window: opens this many minutes before (default; 0–30 allowed), closes 30 after the end.
pub(crate) const JOIN_BEFORE_DEFAULT: i64 = 10;
pub(crate) const JOIN_AFTER_SECS: i64 = 30 * 60;
/// A host may keep a room open at most this long past the scheduled end.
pub(crate) const MAX_EXTENSION_SECS: i64 = 2 * 3600;
/// Text bounds.
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 8_000;
const MAX_LOCATION: usize = 500;
pub(crate) const MAX_NOTE: usize = 300;
const MAX_NAME: usize = 80;

/// The vault label every calendar seal uses.
const SEAL_LABEL: &str = "calendar-v1";

pub(crate) const COLORS: &[&str] = &["blue", "green", "orange", "red", "purple", "teal", "gray"];
/// Meeting codes: no 0/O, 1/I/L — read aloud and typed. Ten characters, so they can never collide
/// with the calls service's six-character ad-hoc codes.
pub(crate) const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub(crate) const CODE_LEN: usize = 10;

// ---- runtime -----------------------------------------------------------------------------------

/// Calendar machinery that lives for the process: the outgoing-message client and the worker's
/// wakeup.
pub(crate) struct Runtime {
    pub notifier: mail::Notifier,
    pub nudge: tokio::sync::Notify,
    /// Public base of the inbox, for links in messages (`https://inbox.pigeonpost.dev`).
    pub inbox_url: String,
    /// The postbox's own public URL, for feed links.
    pub postbox_url: String,
}

impl Runtime {
    pub(crate) fn from_env(postbox_url: &str) -> Self {
        Runtime {
            notifier: mail::Notifier::from_env(),
            nudge: tokio::sync::Notify::new(),
            inbox_url: std::env::var("POSTBOX_INBOX_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://inbox.pigeonpost.dev".into())
                .trim_end_matches('/')
                .to_string(),
            postbox_url: postbox_url.trim_end_matches('/').to_string(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Runtime {
            notifier: mail::Notifier::capture(),
            nudge: tokio::sync::Notify::new(),
            inbox_url: "https://inbox.test".into(),
            postbox_url: "http://postbox.test".into(),
        }
    }
}

// ---- identifiers ------------------------------------------------------------------------------

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", rand_b32(16))
}

fn valid_prefixed(id: &str, prefix: &str) -> bool {
    id.len() == prefix.len() + 27
        && id.starts_with(prefix)
        && id.as_bytes()[prefix.len()] == b'_'
        && id[prefix.len() + 1..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}
pub(crate) fn valid_event_id(id: &str) -> bool {
    valid_prefixed(id, "evt")
}
pub(crate) fn valid_attendee_id(id: &str) -> bool {
    valid_prefixed(id, "att")
}

/// An occurrence id names its event and its recurrence key, so it is stable whether or not the
/// occurrence was ever stored: `occ_<event suffix>_<key>`.
pub(crate) fn occurrence_id(event_id: &str, key: &str) -> String {
    format!("occ_{}_{}", &event_id[4..], key)
}

/// `(event id, recurrence key)` of a well-formed occurrence id.
pub(crate) fn parse_occurrence_id(id: &str) -> Option<(String, String)> {
    let rest = id.strip_prefix("occ_")?;
    let (suffix, key) = rest.split_once('_')?;
    let event = format!("evt_{suffix}");
    if !valid_event_id(&event) {
        return None;
    }
    let ok_key = time::parse_key_timed(key).is_some() || time::parse_key_date(key).is_some();
    ok_key.then(|| (event, key.to_string()))
}

pub(crate) fn new_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    bytes
        .iter()
        .map(|b| CODE_ALPHABET[*b as usize % CODE_ALPHABET.len()] as char)
        .collect()
}

pub(crate) fn valid_code(code: &str) -> bool {
    code.len() == CODE_LEN && code.bytes().all(|b| CODE_ALPHABET.contains(&b))
}

/// A 256-bit bearer secret, base64url.
pub(crate) fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    b64url(&bytes)
}

/// base64url without padding.
pub(crate) fn b64url(bytes: &[u8]) -> String {
    crate::b64_encode(bytes)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

pub(crate) fn unb64url(text: &str) -> Option<Vec<u8>> {
    if !text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    crate::b64_decode(&text.replace('-', "+").replace('_', "/")).ok()
}

pub(crate) fn secret_hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

// ---- sealing and keyed hashes ------------------------------------------------------------------

pub(crate) fn open(vault: &crate::vault::Vault, aad: &str, blob: &[u8]) -> String {
    vault
        .open_labeled(SEAL_LABEL, aad.as_bytes(), blob)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

pub(crate) fn seal_v(vault: &crate::vault::Vault, aad: &str, text: &str) -> GResult<Vec<u8>> {
    vault
        .seal_labeled(SEAL_LABEL, aad.as_bytes(), text.as_bytes())
        .map_err(|_| {
            GErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "seal failed",
            )
        })
}

/// Keyed HMAC of a canonical contact address (SDS §14B.4: not a dictionary-reversible digest).
pub(crate) fn contact_hash(vault: &crate::vault::Vault, canonical: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let key = vault.subkey("calendar-contact-hash-v1");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("any key length");
    mac.update(canonical.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Canonicalize a contact address conservatively: trim, lower-case the domain only (no
/// provider-specific dot or plus rewriting, no local-part case folding). `None` if it is not a
/// plausible address.
pub(crate) fn canonical_contact(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() > 254 || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (local, domain) = s.rsplit_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || local.contains(['<', '>', '(', ')', '[', ']', ',', ';', ':', '"', '\\'])
        || local.starts_with('.')
        || local.ends_with('.')
    {
        return None;
    }
    let domain = domain.to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|l| {
            l.is_empty()
                || l.len() > 63
                || l.starts_with('-')
                || l.ends_with('-')
                || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    Some(format!("{local}@{domain}"))
}

/// Clean free text: NFC is not available without a dependency, so strip controls (keeping
/// newlines where allowed) and bidi overrides, and bound the length.
pub(crate) fn clean_text(raw: &str, max: usize, newlines: bool) -> String {
    let mut out: String = raw
        .chars()
        .filter(|c| {
            !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
                && (!c.is_control() || (newlines && *c == '\n'))
        })
        .collect();
    out = out.trim().to_string();
    if out.chars().count() > max {
        out = out.chars().take(max).collect();
    }
    out
}

pub(crate) fn clean_name(raw: &str) -> String {
    clean_text(raw, MAX_NAME, false)
}

// ---- request context ---------------------------------------------------------------------------

pub(crate) struct Ctx {
    pub principal: Principal,
    pub me: store::StoredIdentity,
}

impl Ctx {
    pub(crate) fn address(&self) -> String {
        self.me.address.clone()
    }
    pub(crate) fn account(&self) -> Option<String> {
        self.me.account_id.clone()
    }
    /// A person at a keyboard (member session), as opposed to an API key, `cap_` token or
    /// connector.
    pub(crate) fn human(&self) -> bool {
        crate::principal::require_human(&self.principal).is_ok()
    }
    /// A person with a realm-verified sign-in address: what sending to contact addresses needs.
    pub(crate) fn human_with_verified_address(&self) -> bool {
        matches!(
            &self.principal,
            Principal::Account(_, crate::AccountCredential::Member(s))
                if s.is_interactive() && s.address_verified
        )
    }
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Resolve the acting mailbox and require the `calendar` flag for its account.
pub(crate) async fn ctx(
    state: &AppState,
    headers: &HeaderMap,
    identity: Option<&str>,
) -> GResult<Ctx> {
    let explicit = identity.or_else(|| header_value(headers, "x-pigeonpost-identity"));
    let (principal, me) = mailbox_for(state, headers, explicit, Action::CalendarUse).await?;
    if !feature_on(state, me.account_id.as_deref(), capabilities::CALENDAR).await {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "the calendar is not enabled for this account",
        ));
    }
    Ok(Ctx { principal, me })
}

// ---- rows --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MeetingOpts {
    pub waiting: String,
    pub admit: String,
    pub allow_knock: bool,
    pub join_before_minutes: i64,
    pub guests_see_list: bool,
    /// Attendee ids allowed to admit when `admit = delegated`.
    #[serde(default)]
    pub admitters: Vec<String>,
}

impl MeetingOpts {
    pub(crate) fn defaults() -> Self {
        MeetingOpts {
            waiting: "guests".into(),
            admit: "hosts".into(),
            allow_knock: false,
            join_before_minutes: JOIN_BEFORE_DEFAULT,
            guests_see_list: false,
            admitters: Vec::new(),
        }
    }
}

/// One event, unsealed.
#[derive(Debug, Clone)]
pub(crate) struct Ev {
    pub id: String,
    pub calendar_id: String,
    pub organizer: String,
    pub revision: i64,
    pub sequence: i64,
    pub title: String,
    pub description: String,
    pub location: String,
    pub all_day: bool,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub local_start: Option<NaiveDateTime>,
    pub local_end: Option<NaiveDateTime>,
    pub tzid: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub rrule: Option<String>,
    pub exdates: Vec<String>,
    pub transparency: String,
    pub privacy: String,
    pub meeting_id: Option<String>,
    pub meeting_code: Option<String>,
    pub meeting: Option<MeetingOpts>,
    pub policy_version: i64,
    pub transport: String,
    pub participant_limit: i64,
    pub linked_group: Option<String>,
    pub locale: String,
    pub status: String,
    pub created_by_account: Option<String>,
    pub split_from: Option<String>,
    #[allow(dead_code)]
    pub span_from: i64,
    pub span_to: Option<i64>,
}

const EV_COLS: &str = "e.id, e.calendar_id, e.organizer, e.revision, e.sequence, e.title_sealed,
    e.details_sealed, e.value_type, e.start_date, e.end_date_exclusive, e.local_start, e.local_end,
    e.tzid, e.starts_at, e.ends_at, e.rrule, e.exdates, e.transparency, e.privacy, e.meeting_id,
    m.code, e.meeting_options, e.meeting_policy_version, e.transport_profile, e.participant_limit,
    e.linked_conversation, e.locale, e.status, e.created_by_account, e.split_from, e.span_from,
    e.span_to";

pub(crate) fn ev_select(where_clause: &str) -> String {
    format!(
        "SELECT {EV_COLS} FROM events e LEFT JOIN meeting_reservations m ON m.id = e.meeting_id \
         WHERE {where_clause}"
    )
}

pub(crate) fn read_ev(vault: &crate::vault::Vault, r: &rusqlite::Row<'_>) -> rusqlite::Result<Ev> {
    let id: String = r.get(0)?;
    let title_sealed: Vec<u8> = r.get(5)?;
    let details_sealed: Option<Vec<u8>> = r.get(6)?;
    let details: Value = details_sealed
        .map(|b| {
            serde_json::from_str(&open(vault, &format!("{id}:details"), &b)).unwrap_or(Value::Null)
        })
        .unwrap_or(Value::Null);
    let value_type: String = r.get(7)?;
    let date = |s: Option<String>| s.and_then(|s| time::parse_date(&s).ok());
    let local = |s: Option<String>| s.and_then(|s| time::parse_local(&s).ok());
    let meeting: Option<MeetingOpts> = r
        .get::<_, Option<String>>(21)?
        .and_then(|s| serde_json::from_str(&s).ok());
    let meeting_id: Option<String> = r.get(19)?;
    Ok(Ev {
        title: open(vault, &format!("{id}:title"), &title_sealed),
        description: details["description"].as_str().unwrap_or("").to_string(),
        location: details["location"].as_str().unwrap_or("").to_string(),
        calendar_id: r.get(1)?,
        organizer: r.get(2)?,
        revision: r.get(3)?,
        sequence: r.get(4)?,
        all_day: value_type == "date",
        start_date: date(r.get(8)?),
        end_date: date(r.get(9)?),
        local_start: local(r.get(10)?),
        local_end: local(r.get(11)?),
        tzid: r.get(12)?,
        starts_at: r.get(13)?,
        ends_at: r.get(14)?,
        rrule: r.get(15)?,
        exdates: r
            .get::<_, Option<String>>(16)?
            .map(|s| {
                s.split(',')
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        transparency: r.get(17)?,
        privacy: r.get(18)?,
        meeting: if meeting_id.is_some() { meeting } else { None },
        meeting_id,
        meeting_code: r.get(20)?,
        policy_version: r.get(22)?,
        transport: r
            .get::<_, Option<String>>(23)?
            .unwrap_or_else(|| "mesh".into()),
        participant_limit: r.get::<_, Option<i64>>(24)?.unwrap_or(MESH_LIMIT),
        linked_group: r.get(25)?,
        locale: r.get(26)?,
        status: r.get(27)?,
        created_by_account: r.get(28)?,
        split_from: r.get(29)?,
        span_from: r.get(30)?,
        span_to: r.get(31)?,
        id,
    })
}

pub(crate) fn load_ev(
    c: &Connection,
    vault: &crate::vault::Vault,
    id: &str,
) -> GResult<Option<Ev>> {
    Ok(
        c.query_row(&ev_select("e.id = ?1"), params![id], |r| read_ev(vault, r))
            .optional()?,
    )
}

impl Ev {
    pub(crate) fn tz(&self) -> Tz {
        self.tzid
            .as_deref()
            .and_then(|t| time::parse_tz(t).ok())
            .unwrap_or(Tz::UTC)
    }

    pub(crate) fn series(&self) -> time::Series {
        let (start, length) = if self.all_day {
            let s = self.start_date.unwrap_or_default();
            let e = self.end_date.unwrap_or(s + Duration::days(1));
            (time::midnight(s), e - s)
        } else {
            let s = self.local_start.unwrap_or_default();
            let e = self.local_end.unwrap_or(s + Duration::minutes(30));
            (s, e - s)
        };
        time::Series {
            all_day: self.all_day,
            start,
            length,
            tz: self.tz(),
            rule: self
                .rrule
                .as_deref()
                .and_then(|r| time::parse_rule(r, self.all_day, start).ok()),
            exdates: self.exdates.iter().cloned().collect(),
        }
    }

    pub(crate) fn uid(&self) -> String {
        format!("{}@pigeonpost.dev", self.id)
    }

    pub(crate) fn recurring(&self) -> bool {
        self.rrule.is_some()
    }

    pub(crate) fn meeting_url(&self, inbox: &str) -> Option<String> {
        self.meeting_code.as_ref().map(|c| format!("{inbox}/m/{c}"))
    }

    /// The non-authorizing locator used in guest links: the meeting code, or the event id.
    pub(crate) fn locator(&self) -> String {
        self.meeting_code.clone().unwrap_or_else(|| self.id.clone())
    }
}

/// A per-occurrence change.
#[derive(Debug, Clone, Default)]
pub(crate) struct Override {
    pub occurrence_id: String,
    pub key: String,
    pub local_start: Option<NaiveDateTime>,
    pub local_end: Option<NaiveDateTime>,
    pub tzid: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub status: Option<String>,
    pub sequence: i64,
}

pub(crate) fn load_overrides(
    c: &Connection,
    vault: &crate::vault::Vault,
    event_id: &str,
) -> GResult<HashMap<String, Override>> {
    let mut stmt = c.prepare(
        "SELECT o.occurrence_id, x.recurrence_key, o.local_start, o.local_end, o.tzid,
                o.start_date, o.end_date_exclusive, o.title_sealed, o.details_sealed, o.status,
                o.sequence
           FROM event_overrides o JOIN event_occurrences x ON x.id = o.occurrence_id
          WHERE x.event_id = ?1",
    )?;
    let rows = stmt.query_map(params![event_id], |r| {
        let occ: String = r.get(0)?;
        let title: Option<Vec<u8>> = r.get(7)?;
        let details: Option<Vec<u8>> = r.get(8)?;
        let details: Value = details
            .map(|b| {
                serde_json::from_str(&open(vault, &format!("{occ}:details"), &b))
                    .unwrap_or(Value::Null)
            })
            .unwrap_or(Value::Null);
        Ok(Override {
            title: title.map(|b| open(vault, &format!("{occ}:title"), &b)),
            description: details["description"].as_str().map(str::to_string),
            location: details["location"].as_str().map(str::to_string),
            occurrence_id: occ,
            key: r.get(1)?,
            local_start: r
                .get::<_, Option<String>>(2)?
                .and_then(|s| time::parse_local(&s).ok()),
            local_end: r
                .get::<_, Option<String>>(3)?
                .and_then(|s| time::parse_local(&s).ok()),
            tzid: r.get(4)?,
            start_date: r
                .get::<_, Option<String>>(5)?
                .and_then(|s| time::parse_date(&s).ok()),
            end_date: r
                .get::<_, Option<String>>(6)?
                .and_then(|s| time::parse_date(&s).ok()),
            status: r.get(9)?,
            sequence: r.get(10)?,
        })
    })?;
    let mut out = HashMap::new();
    for o in rows {
        let o = o?;
        out.insert(o.key.clone(), o);
    }
    Ok(out)
}

/// An occurrence as it actually is: generated, then changed by its override.
#[derive(Debug, Clone)]
pub(crate) struct Occ {
    pub id: String,
    pub key: String,
    pub all_day: bool,
    pub starts_at: i64,
    pub ends_at: i64,
    pub local_start: NaiveDateTime,
    pub local_end: NaiveDateTime,
    pub tzid: Option<String>,
    pub status: String,
    pub overridden: bool,
    pub title: String,
    pub description: String,
    pub location: String,
    pub sequence: i64,
}

impl Occ {
    pub(crate) fn start_date(&self) -> NaiveDate {
        self.local_start.date()
    }
    pub(crate) fn end_date(&self) -> NaiveDate {
        self.local_end.date()
    }
}

pub(crate) fn effective(ev: &Ev, inst: &time::Instance, ov: Option<&Override>) -> Occ {
    let mut occ = Occ {
        id: occurrence_id(&ev.id, &inst.key),
        key: inst.key.clone(),
        all_day: ev.all_day,
        starts_at: inst.starts_at,
        ends_at: inst.ends_at,
        local_start: inst.local_start,
        local_end: inst.local_end,
        tzid: ev.tzid.clone(),
        status: ev.status.clone(),
        overridden: false,
        title: ev.title.clone(),
        description: ev.description.clone(),
        location: ev.location.clone(),
        sequence: ev.sequence,
    };
    if let Some(o) = ov {
        occ.overridden = true;
        occ.sequence = o.sequence;
        if ev.all_day {
            if let (Some(s), Some(e)) = (o.start_date, o.end_date) {
                occ.local_start = time::midnight(s);
                occ.local_end = time::midnight(e);
                occ.starts_at = occ.local_start.and_utc().timestamp();
                occ.ends_at = occ.local_end.and_utc().timestamp();
            }
        } else if let (Some(s), Some(e)) = (o.local_start, o.local_end) {
            let tz = o
                .tzid
                .as_deref()
                .and_then(|t| time::parse_tz(t).ok())
                .unwrap_or_else(|| ev.tz());
            occ.local_start = s;
            occ.local_end = e;
            occ.tzid = Some(tz.name().to_string());
            occ.starts_at = time::resolve_rfc(s, tz);
            occ.ends_at = time::resolve_rfc(e, tz).max(occ.starts_at + 60);
        }
        if let Some(t) = &o.title {
            occ.title = t.clone();
        }
        if let Some(d) = &o.description {
            occ.description = d.clone();
        }
        if let Some(l) = &o.location {
            occ.location = l.clone();
        }
        if ev.status == "confirmed" {
            if let Some(s) = &o.status {
                occ.status = s.clone();
            }
        }
    }
    occ
}

/// One occurrence by key, whether or not the series still generates it (`None` if it does not
/// and was never stored).
pub(crate) fn occurrence_by_key(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    key: &str,
) -> GResult<Option<Occ>> {
    let series = ev.series();
    let overrides = load_overrides(c, vault, &ev.id)?;
    let start = if ev.all_day {
        time::parse_key_date(key).map(time::midnight)
    } else {
        time::parse_key_timed(key)
    };
    let Some(start) = start else { return Ok(None) };
    let ov = overrides.get(key);
    if !series.generates(key) && ov.is_none() {
        return Ok(None);
    }
    if series.exdates.contains(key) && ov.is_none() {
        return Ok(None);
    }
    Ok(Some(effective(ev, &series.instance_at(start), ov)))
}

/// Effective occurrences of `ev` overlapping `[from, to)`, including moved ones whose new time
/// falls in the window. Cancelled ones are included (callers filter).
pub(crate) fn occurrences_in(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    from: i64,
    to: i64,
    limit: usize,
) -> GResult<Vec<Occ>> {
    let overrides = load_overrides(c, vault, &ev.id)?;
    let series = ev.series();
    let instances = series.expand(from, to, limit).map_err(|e| match e {
        time::ExpandError::Limit => GErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "expansion_limit",
            "too many occurrences in this window; ask for a shorter one",
        ),
        time::ExpandError::Invalid(why) => {
            tracing::error!(event = %ev.id, %why, "stored rule no longer parses");
            GErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "store_error",
            )
        }
    })?;
    let slack = if ev.all_day { 14 * 3600 } else { 0 };
    let inside = |o: &Occ| o.starts_at < to + slack && o.ends_at > from - slack;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for inst in &instances {
        let occ = effective(ev, inst, overrides.get(&inst.key));
        seen.insert(occ.key.clone());
        if inside(&occ) {
            out.push(occ);
        }
    }
    // Moved in from outside the window.
    for (key, ov) in &overrides {
        if seen.contains(key) || series.exdates.contains(key) {
            continue;
        }
        let start = if ev.all_day {
            time::parse_key_date(key).map(time::midnight)
        } else {
            time::parse_key_timed(key)
        };
        let Some(start) = start else { continue };
        let occ = effective(ev, &series.instance_at(start), Some(ov));
        if inside(&occ) && series.generates(key) {
            out.push(occ);
        }
    }
    out.sort_by_key(|o| (o.starts_at, o.key.clone()));
    Ok(out)
}

/// Store an occurrence row (idempotent), refreshing its effective times.
pub(crate) fn ensure_occurrence(c: &Connection, ev: &Ev, occ: &Occ) -> GResult<()> {
    let (s, e, sd, ed) = if ev.all_day {
        (
            None,
            None,
            Some(time::fmt_date(occ.start_date())),
            Some(time::fmt_date(occ.end_date())),
        )
    } else {
        (Some(occ.starts_at), Some(occ.ends_at), None, None)
    };
    c.execute(
        "INSERT INTO event_occurrences (id, event_id, recurrence_key, event_revision, starts_at,
                                        ends_at, start_date, end_date_exclusive, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(event_id, recurrence_key) DO UPDATE SET
            event_revision = excluded.event_revision, starts_at = excluded.starts_at,
            ends_at = excluded.ends_at, start_date = excluded.start_date,
            end_date_exclusive = excluded.end_date_exclusive, status = excluded.status",
        params![
            occ.id,
            ev.id,
            occ.key,
            ev.revision,
            s,
            e,
            sd,
            ed,
            occ.status
        ],
    )?;
    Ok(())
}

/// One attendee row.
#[derive(Debug, Clone)]
pub(crate) struct Att {
    pub id: String,
    pub event_id: String,
    pub kind: String,
    pub address: Option<String>,
    pub contact_sealed: Option<Vec<u8>>,
    pub contact_hash: Option<Vec<u8>>,
    pub display_name: Option<String>,
    pub role: String,
    pub optional: bool,
    pub base: String,
    pub rsvp: String,
    pub rsvp_note_sealed: Option<Vec<u8>>,
    pub rsvp_at: Option<i64>,
    pub rsvp_proof: Option<String>,
    pub revision: i64,
    pub grant_version: i64,
    pub calendar_id: Option<String>,
    pub filtered: bool,
    pub suppressed: bool,
    #[allow(dead_code)]
    pub can_admit: bool,
    pub added_at: i64,
    pub removed_at: Option<i64>,
    pub redacted_at: Option<i64>,
}

const ATT_COLS: &str = "id, event_id, kind, address, contact_sealed, contact_hash, display_name,
    role, optional, base_participation, rsvp, rsvp_note_sealed, rsvp_at, rsvp_proof, revision,
    grant_version, calendar_id, filtered, suppressed, can_admit, added_at, removed_at, redacted_at";

pub(crate) fn att_select(where_clause: &str) -> String {
    format!("SELECT {ATT_COLS} FROM event_attendees WHERE {where_clause}")
}

pub(crate) fn read_att(r: &rusqlite::Row<'_>) -> rusqlite::Result<Att> {
    Ok(Att {
        id: r.get(0)?,
        event_id: r.get(1)?,
        kind: r.get(2)?,
        address: r.get(3)?,
        contact_sealed: r.get(4)?,
        contact_hash: r.get(5)?,
        display_name: r.get(6)?,
        role: r.get(7)?,
        optional: r.get::<_, i64>(8)? != 0,
        base: r.get(9)?,
        rsvp: r.get(10)?,
        rsvp_note_sealed: r.get(11)?,
        rsvp_at: r.get(12)?,
        rsvp_proof: r.get(13)?,
        revision: r.get(14)?,
        grant_version: r.get(15)?,
        calendar_id: r.get(16)?,
        filtered: r.get::<_, i64>(17)? != 0,
        suppressed: r.get::<_, i64>(18)? != 0,
        can_admit: r.get::<_, i64>(19)? != 0,
        added_at: r.get(20)?,
        removed_at: r.get(21)?,
        redacted_at: r.get(22)?,
    })
}

pub(crate) fn live_attendees(c: &Connection, event_id: &str) -> GResult<Vec<Att>> {
    let mut stmt = c.prepare(&att_select(
        "event_id = ?1 AND removed_at IS NULL ORDER BY added_at, id",
    ))?;
    let rows = stmt
        .query_map(params![event_id], read_att)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn attendee(c: &Connection, id: &str) -> GResult<Option<Att>> {
    Ok(c.query_row(&att_select("id = ?1"), params![id], read_att)
        .optional()?)
}

/// The caller's live invitation to an event, if any.
pub(crate) fn my_attendance(c: &Connection, event_id: &str, address: &str) -> GResult<Option<Att>> {
    Ok(c.query_row(
        &att_select("event_id = ?1 AND address = ?2 AND kind = 'handle' AND removed_at IS NULL"),
        params![event_id, address],
        read_att,
    )
    .optional()?)
}

/// Per-occurrence attendance overrides.
#[derive(Debug, Clone, Default)]
pub(crate) struct OccAtt {
    pub participation: Option<String>,
    pub rsvp: Option<String>,
    pub role: Option<String>,
    #[allow(dead_code)]
    pub rsvp_at: Option<i64>,
}

pub(crate) fn occ_att(c: &Connection, occurrence: &str, attendee: &str) -> GResult<Option<OccAtt>> {
    Ok(c.query_row(
        "SELECT participation, rsvp, role, rsvp_at FROM event_occurrence_attendees
              WHERE occurrence_id = ?1 AND attendee_id = ?2",
        params![occurrence, attendee],
        |r| {
            Ok(OccAtt {
                participation: r.get(0)?,
                rsvp: r.get(1)?,
                role: r.get(2)?,
                rsvp_at: r.get(3)?,
            })
        },
    )
    .optional()?)
}

/// What an attendee is for one occurrence: invited or not, their answer, their role.
#[derive(Debug, Clone)]
pub(crate) struct Standing {
    pub invited: bool,
    pub rsvp: String,
    pub role: String,
}

pub(crate) fn standing(c: &Connection, att: &Att, occurrence: Option<&str>) -> GResult<Standing> {
    let mut s = Standing {
        invited: att.base == "invited" && att.removed_at.is_none(),
        rsvp: att.rsvp.clone(),
        role: att.role.clone(),
    };
    if let Some(occ) = occurrence {
        if let Some(o) = occ_att(c, occ, &att.id)? {
            if let Some(p) = o.participation {
                s.invited = p == "invited" && att.removed_at.is_none();
            }
            if let Some(r) = o.rsvp {
                s.rsvp = r;
            }
            if let Some(r) = o.role {
                if att.kind == "handle" {
                    s.role = r;
                }
            }
        }
    }
    Ok(s)
}

// ---- calendars ---------------------------------------------------------------------------------

/// The mailbox's default calendar, created on first use.
pub(crate) fn default_calendar(c: &Connection, mailbox: &str, now: u64) -> GResult<String> {
    if let Some(id) = c
        .query_row(
            "SELECT id FROM calendars WHERE owner_identity = ?1 AND is_default = 1 AND deleted_at IS NULL",
            params![mailbox],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let id = new_id("cal");
    c.execute(
        "INSERT INTO calendars (id, owner_identity, name, color, is_default, created_at)
         VALUES (?1, ?2, 'Calendar', 'blue', 1, ?3)",
        params![id, mailbox, now as i64],
    )?;
    Ok(id)
}

/// A live calendar owned by `mailbox`.
pub(crate) fn own_calendar(c: &Connection, mailbox: &str, id: &str) -> GResult<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM calendars WHERE id = ?1 AND owner_identity = ?2 AND deleted_at IS NULL",
        params![id, mailbox],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

// ---- accounts and identities -------------------------------------------------------------------

pub(crate) fn account_of(c: &Connection, mailbox: &str) -> GResult<Option<String>> {
    Ok(c.query_row(
        "SELECT account_id FROM identities WHERE address = ?1",
        params![mailbox],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}

pub(crate) fn handle_of(c: &Connection, mailbox: &str) -> GResult<Option<String>> {
    Ok(c.query_row(
        "SELECT handle FROM identities WHERE address = ?1",
        params![mailbox],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}

pub(crate) fn person(c: &Connection, mailbox: &str) -> GResult<Value> {
    Ok(json!({ "address": mailbox, "handle": handle_of(c, mailbox)? }))
}

// ---- effects -----------------------------------------------------------------------------------

/// What to do after a transaction commits.
#[derive(Default)]
pub(crate) struct Effects {
    pub wake: Vec<String>,
    pub push: Vec<(String, push::Notification)>,
    pub nudge: bool,
}

impl Effects {
    /// Record an account event for `mailbox` (if it belongs to an account) and wake its streams.
    pub(crate) fn event(
        &mut self,
        c: &Connection,
        mailbox: &str,
        kind: &str,
        data: Value,
        now: u64,
    ) -> GResult<()> {
        if let Some(account) = account_of(c, mailbox)? {
            store::append_event(c, &account, kind, Some(mailbox), &data, now)?;
            self.wake.push(account);
        }
        Ok(())
    }

    pub(crate) fn invitation_push(&mut self, mailbox: &str, organizer: &str, event_id: &str) {
        self.push.push((
            mailbox.to_string(),
            push::Notification {
                title: "Calendar".into(),
                subtitle: mailbox.to_string(),
                body: "New calendar invitation".into(),
                message_id: format!("cal:{event_id}:invite"),
                peer: organizer.to_string(),
                unread: 0,
            },
        ));
    }
}

/// Perform effects and build the response.
pub(crate) fn finish(
    state: &AppState,
    result: GResult<(u16, Value, Effects)>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match result {
        Ok((status, body, fx)) => {
            run_effects(state, fx);
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            (status, axum::Json(body)).into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub(crate) fn run_effects(state: &AppState, fx: Effects) {
    let mut woke = HashSet::new();
    for account in fx.wake {
        if woke.insert(account.clone()) {
            state.event_hub.wake(&account);
        }
    }
    if fx.nudge {
        state.calendar.nudge.notify_one();
    }
    if !fx.push.is_empty() && (state.apns.is_some() || state.fcm.is_some()) {
        let state = state.clone();
        tokio::spawn(async move {
            for (mailbox, note) in fx.push {
                push::fan_out(
                    state.apns.clone(),
                    state.fcm.clone(),
                    state.store.clone(),
                    mailbox,
                    note,
                )
                .await;
            }
        });
    }
}

// ---- idempotency -------------------------------------------------------------------------------

pub(crate) const IDEMPOTENCY_SECS: u64 = 7 * 86_400;

/// Replay a recorded response for `(scope, action, key)`, refuse the same key with another body,
/// or run `f` and record its response — inside the caller's transaction.
pub(crate) fn idempotent(
    c: &Connection,
    scope: &str,
    action: &str,
    key: &str,
    body_hash: &str,
    now: u64,
    f: impl FnOnce() -> GResult<(u16, Value, Effects)>,
) -> GResult<(u16, Value, Effects)> {
    if !crate::groups::valid_key(key) {
        return Err(GErr::bad(
            "bad_request",
            "request_key must be 1-64 characters of A-Z a-z 0-9 _ -",
        ));
    }
    let prior: Option<(String, i64, String)> = c
        .query_row(
            "SELECT body_hash, status, response FROM calendar_idempotency
              WHERE scope = ?1 AND action = ?2 AND request_key = ?3 AND created_at > ?4",
            params![
                scope,
                action,
                key,
                now.saturating_sub(IDEMPOTENCY_SECS) as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((hash, status, response)) = prior {
        if hash != body_hash {
            return Err(GErr::conflict(
                "idempotency_conflict",
                "that request_key was used with a different request",
            ));
        }
        return Ok((
            status as u16,
            serde_json::from_str(&response).unwrap_or(Value::Null),
            Effects::default(),
        ));
    }
    let out = f()?;
    c.execute(
        "INSERT OR REPLACE INTO calendar_idempotency
            (scope, action, request_key, body_hash, status, response, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            scope,
            action,
            key,
            body_hash,
            out.0 as i64,
            out.1.to_string(),
            now as i64
        ],
    )?;
    Ok(out)
}

pub(crate) fn conflict(current: i64) -> GErr {
    GErr::conflict(
        "revision_conflict",
        "the event changed since you loaded it; reload and try again",
    )
    .with("current_revision", current)
}

pub(crate) fn not_found() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "not_found", "no such event")
}

/// Map a time validation failure to the API error.
pub(crate) fn time_err(e: time::TimeError) -> GErr {
    match e {
        time::TimeError::Invalid(m) => GErr::bad("time_invalid", m).with("kind", "invalid"),
        time::TimeError::Nonexistent => GErr::bad(
            "time_invalid",
            "that time does not exist on that day in that time zone (the clocks skip it)",
        )
        .with("kind", "nonexistent"),
        time::TimeError::Ambiguous => GErr::bad(
            "time_invalid",
            "that time happens twice on that day in that time zone; choose the earlier or later one",
        )
        .with("kind", "ambiguous"),
        time::TimeError::Order => {
            GErr::bad("time_invalid", "the end must be after the start").with("kind", "order")
        }
        time::TimeError::ZoneUnknown => GErr::bad("zone_unknown", "unknown time zone"),
    }
}

/// Join window of an occurrence, `(opens_at, closes_at)`.
pub(crate) fn window(ev: &Ev, occ: &Occ, extended_until: Option<i64>) -> (i64, i64) {
    let before = ev
        .meeting
        .as_ref()
        .map(|m| m.join_before_minutes)
        .unwrap_or(JOIN_BEFORE_DEFAULT)
        .clamp(0, 30);
    let close = occ.ends_at + JOIN_AFTER_SECS;
    let close = extended_until
        .map(|x| x.min(occ.ends_at + MAX_EXTENSION_SECS).max(close))
        .unwrap_or(close);
    (occ.starts_at - before * 60, close)
}

pub(crate) fn extended_until(c: &Connection, occurrence: &str) -> GResult<Option<i64>> {
    Ok(c.query_row(
        "SELECT extended_until FROM event_occurrences WHERE id = ?1",
        params![occurrence],
        |r| r.get::<_, Option<i64>>(0),
    )
    .optional()?
    .flatten())
}
