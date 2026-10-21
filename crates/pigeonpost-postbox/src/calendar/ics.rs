//! iCalendar (RFC 5545) writing with iMIP conventions (RFC 6047), and bounded, network-free
//! reading for `.ics` import (SDS §14B.6, §14B.10).
//!
//! What is written never carries a credential: `URL` is the meeting locator (a code is not a
//! credential), there is no invitation link, no token, no attendee list — only the recipient's own
//! `ATTENDEE` line — and `RSVP=FALSE`, because native calendar replies are not processed in v1
//! (Pigeonpost links are authoritative).

use super::time;
use super::*;
use chrono::{DateTime, Utc};

const PRODID: &str = "-//Pigeonpost//Calendar 1.0//EN";

/// Escape a TEXT value (RFC 5545 §3.3.11).
pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A parameter value: quoted, with characters that cannot appear in a quoted string removed.
fn param(s: &str) -> String {
    let clean: String = s
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .take(80)
        .collect();
    format!("\"{clean}\"")
}

/// Fold a content line at 75 octets without splitting a UTF-8 sequence.
pub(crate) fn fold(line: &str, out: &mut String) {
    let mut count = 0;
    for ch in line.chars() {
        let n = ch.len_utf8();
        if count + n > 75 {
            out.push_str("\r\n ");
            count = 1;
        }
        out.push(ch);
        count += n;
    }
    out.push_str("\r\n");
}

struct Writer {
    out: String,
}

impl Writer {
    fn new() -> Self {
        Writer { out: String::new() }
    }
    fn line(&mut self, s: impl AsRef<str>) {
        fold(s.as_ref(), &mut self.out);
    }
}

pub(crate) fn utc_stamp(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .unwrap_or_default()
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn offset_str(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    format!("{sign}{:02}{:02}", a / 3600, (a % 3600) / 60)
}

/// A VTIMEZONE covering `[from_year, to_year]` with explicit transitions (no RRULEs to
/// misinterpret). A zone without transitions gets one STANDARD block.
fn vtimezone(w: &mut Writer, tz: Tz, from_year: i32, to_year: i32) {
    w.line("BEGIN:VTIMEZONE");
    w.line(format!("TZID:{}", tz.name()));
    let ts = time::transitions(tz, from_year, to_year);
    if ts.is_empty() {
        let probe = time::midnight(NaiveDate::from_ymd_opt(from_year, 1, 1).unwrap_or_default())
            .and_utc()
            .timestamp();
        let off = time::offset_at(probe, tz);
        w.line("BEGIN:STANDARD");
        w.line("DTSTART:19700101T000000");
        w.line(format!("TZOFFSETFROM:{}", offset_str(off)));
        w.line(format!("TZOFFSETTO:{}", offset_str(off)));
        w.line("END:STANDARD");
    } else {
        for (at, before, after, dst) in ts {
            let kind = if dst { "DAYLIGHT" } else { "STANDARD" };
            w.line(format!("BEGIN:{kind}"));
            // DTSTART is the local time before the change, in the offset before it.
            let local = DateTime::<Utc>::from_timestamp(at + before as i64, 0)
                .map(|d| d.naive_utc())
                .unwrap_or_default();
            w.line(format!("DTSTART:{}", time::key_timed(local)));
            w.line(format!("TZOFFSETFROM:{}", offset_str(before)));
            w.line(format!("TZOFFSETTO:{}", offset_str(after)));
            w.line(format!("END:{kind}"));
        }
    }
    w.line("END:VTIMEZONE");
}

/// How one recipient is shown in their own copy.
pub(crate) struct Recipient {
    pub name: String,
    /// `mailto:` for a contact guest, `urn:pigeonpost:<address>` for a handle attendee.
    pub uri: String,
    pub partstat: String,
}

pub(crate) fn partstat(rsvp: &str) -> String {
    match rsvp {
        "accepted" => "ACCEPTED",
        "tentative" => "TENTATIVE",
        "declined" => "DECLINED",
        _ => "NEEDS-ACTION",
    }
    .into()
}

/// What to put in a calendar file.
pub(crate) struct Doc<'a> {
    pub ev: &'a Ev,
    pub method: &'a str,
    pub recipient: Option<Recipient>,
    /// Only this occurrence (one-occurrence invitations and per-occurrence cancellations).
    pub only: Option<&'a Occ>,
    /// Per-occurrence changes to include with the series.
    pub overrides: Vec<Occ>,
    pub organizer_name: String,
    pub inbox: &'a str,
    /// Redact to a busy block (feeds).
    pub busy_only: bool,
    /// Drop description (feeds).
    pub no_description: bool,
    pub now: i64,
}

fn dt_line(w: &mut Writer, name: &str, ev: &Ev, local: NaiveDateTime, tzid: Option<&str>) {
    if ev.all_day {
        w.line(format!(
            "{name};VALUE=DATE:{}",
            time::key_date(local.date())
        ));
    } else {
        let tz = tzid.or(ev.tzid.as_deref()).unwrap_or("UTC");
        w.line(format!("{name};TZID={tz}:{}", time::key_timed(local)));
    }
}

fn recurrence_id(w: &mut Writer, ev: &Ev, key: &str) {
    if ev.all_day {
        w.line(format!("RECURRENCE-ID;VALUE=DATE:{key}"));
    } else {
        w.line(format!(
            "RECURRENCE-ID;TZID={}:{key}",
            ev.tzid.as_deref().unwrap_or("UTC")
        ));
    }
}

fn common(
    w: &mut Writer,
    d: &Doc<'_>,
    title: &str,
    description: &str,
    location: &str,
    status: &str,
    sequence: i64,
) {
    let ev = d.ev;
    w.line(format!("UID:{}", ev.uid()));
    w.line(format!("DTSTAMP:{}", utc_stamp(d.now)));
    w.line(format!("SEQUENCE:{sequence}"));
    if d.busy_only {
        w.line("SUMMARY:Busy");
        w.line("CLASS:CONFIDENTIAL");
    } else {
        w.line(format!("SUMMARY:{}", esc(title)));
        if !d.no_description && !description.is_empty() {
            w.line(format!("DESCRIPTION:{}", esc(description)));
        }
        if !location.is_empty() {
            w.line(format!("LOCATION:{}", esc(location)));
        }
        if let Some(url) = ev.meeting_url(d.inbox).filter(|_| !d.no_description) {
            w.line(format!("URL:{url}"));
        }
        if ev.privacy == "private" {
            w.line("CLASS:PRIVATE");
        }
        w.line(format!(
            "ORGANIZER;CN={}:mailto:invites@pigeonpost.dev",
            param(&d.organizer_name)
        ));
        if let Some(r) = &d.recipient {
            w.line(format!(
                "ATTENDEE;CN={};ROLE=REQ-PARTICIPANT;PARTSTAT={};RSVP=FALSE:{}",
                param(&r.name),
                r.partstat,
                r.uri
            ));
        }
    }
    w.line(format!(
        "STATUS:{}",
        if status == "cancelled" {
            "CANCELLED"
        } else {
            "CONFIRMED"
        }
    ));
    w.line(format!(
        "TRANSP:{}",
        if ev.transparency == "transparent" {
            "TRANSPARENT"
        } else {
            "OPAQUE"
        }
    ));
}

/// Render a calendar file.
pub(crate) fn render(d: &Doc<'_>) -> String {
    let ev = d.ev;
    let mut w = Writer::new();
    w.line("BEGIN:VCALENDAR");
    w.line(format!("PRODID:{PRODID}"));
    w.line("VERSION:2.0");
    w.line("CALSCALE:GREGORIAN");
    w.line(format!("METHOD:{}", d.method));
    if !ev.all_day {
        let mut zones: Vec<String> = vec![ev.tzid.clone().unwrap_or_else(|| "UTC".into())];
        for o in d.only.iter().copied().chain(d.overrides.iter()) {
            if let Some(t) = &o.tzid {
                if !zones.contains(t) {
                    zones.push(t.clone());
                }
            }
        }
        let first = ev.starts_at.unwrap_or(d.now);
        let y0 = DateTime::<Utc>::from_timestamp(first, 0)
            .unwrap_or_default()
            .year_ce()
            .1 as i32;
        let y1 = match ev.span_to {
            Some(end) => {
                DateTime::<Utc>::from_timestamp(end, 0)
                    .unwrap_or_default()
                    .year_ce()
                    .1 as i32
            }
            None => y0 + 3,
        }
        .min(y0 + 10);
        for z in zones {
            if let Ok(tz) = time::parse_tz(&z) {
                vtimezone(&mut w, tz, y0 - 1, y1 + 1);
            }
        }
    }
    match d.only {
        Some(occ) => {
            w.line("BEGIN:VEVENT");
            common(
                &mut w,
                d,
                &occ.title,
                &occ.description,
                &occ.location,
                &occ.status,
                occ.sequence.max(ev.sequence),
            );
            if ev.recurring() {
                recurrence_id(&mut w, ev, &occ.key);
            }
            dt_line(&mut w, "DTSTART", ev, occ.local_start, occ.tzid.as_deref());
            dt_line(&mut w, "DTEND", ev, occ.local_end, occ.tzid.as_deref());
            w.line("END:VEVENT");
        }
        None => {
            w.line("BEGIN:VEVENT");
            common(
                &mut w,
                d,
                &ev.title,
                &ev.description,
                &ev.location,
                &ev.status,
                ev.sequence,
            );
            let series = ev.series();
            dt_line(&mut w, "DTSTART", ev, series.start, None);
            dt_line(&mut w, "DTEND", ev, series.start + series.length, None);
            if let Some(r) = &ev.rrule {
                w.line(format!("RRULE:{r}"));
                if !ev.exdates.is_empty() {
                    if ev.all_day {
                        w.line(format!("EXDATE;VALUE=DATE:{}", ev.exdates.join(",")));
                    } else {
                        w.line(format!(
                            "EXDATE;TZID={}:{}",
                            ev.tzid.as_deref().unwrap_or("UTC"),
                            ev.exdates.join(",")
                        ));
                    }
                }
            }
            w.line("END:VEVENT");
            for occ in &d.overrides {
                w.line("BEGIN:VEVENT");
                common(
                    &mut w,
                    d,
                    &occ.title,
                    &occ.description,
                    &occ.location,
                    &occ.status,
                    occ.sequence.max(ev.sequence),
                );
                recurrence_id(&mut w, ev, &occ.key);
                dt_line(&mut w, "DTSTART", ev, occ.local_start, occ.tzid.as_deref());
                dt_line(&mut w, "DTEND", ev, occ.local_end, occ.tzid.as_deref());
                w.line("END:VEVENT");
            }
        }
    }
    w.line("END:VCALENDAR");
    w.out
}

trait YearCe {
    fn year_ce(&self) -> (bool, u32);
}
impl YearCe for DateTime<Utc> {
    fn year_ce(&self) -> (bool, u32) {
        chrono::Datelike::year_ce(self)
    }
}

/// Overrides of an event as effective occurrences (for series exports).
pub(crate) fn override_occs(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
) -> GResult<Vec<Occ>> {
    let series = ev.series();
    let mut out = Vec::new();
    for (key, ov) in load_overrides(c, vault, &ev.id)? {
        let start = if ev.all_day {
            time::parse_key_date(&key).map(time::midnight)
        } else {
            time::parse_key_timed(&key)
        };
        if let Some(start) = start {
            out.push(effective(ev, &series.instance_at(start), Some(&ov)));
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

pub(crate) fn organizer_name(c: &Connection, ev: &Ev) -> GResult<String> {
    Ok(handle_of(c, &ev.organizer)?.unwrap_or_else(|| ev.organizer.clone()))
}

/// "Add to my calendar app": the caller's own projection, `METHOD:PUBLISH`.
pub(crate) fn export(
    c: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    att: Option<&Att>,
    inbox: &str,
) -> GResult<String> {
    let recipient = att.map(|a| Recipient {
        name: match &a.address {
            Some(addr) => handle_of(c, addr)
                .ok()
                .flatten()
                .unwrap_or_else(|| addr.clone()),
            None => a.display_name.clone().unwrap_or_else(|| "Guest".into()),
        },
        uri: match &a.address {
            Some(addr) => format!("urn:pigeonpost:{}", addr.trim_start_matches('/')),
            None => a
                .contact_sealed
                .as_ref()
                .map(|b| format!("mailto:{}", open(vault, &format!("{}:contact", a.id), b)))
                .unwrap_or_else(|| "urn:pigeonpost:guest".into()),
        },
        partstat: partstat(&a.rsvp),
    });
    Ok(render(&Doc {
        ev,
        method: "PUBLISH",
        recipient,
        only: None,
        overrides: override_occs(c, vault, ev)?,
        organizer_name: organizer_name(c, ev)?,
        inbox,
        busy_only: false,
        no_description: false,
        now: crate::now_unix() as i64,
    }))
}

// ---- reading -----------------------------------------------------------------------------------

/// Import limits (SDS §14B.10).
pub(crate) const MAX_IMPORT_BYTES: usize = 512 * 1024;
const MAX_IMPORT_EVENTS: usize = 200;
const MAX_LINE: usize = 8 * 1024;
const MAX_VALUE: usize = 4 * 1024;
const MAX_LINES: usize = 50_000;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct Imported {
    pub index: usize,
    pub title: String,
    pub description: String,
    pub location: String,
    pub all_day: bool,
    pub start: Option<String>,
    pub end: Option<String>,
    pub tzid: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Vec<String>,
    pub uid: Option<String>,
    pub warnings: Vec<String>,
    /// False when it cannot be imported as is (unsupported zone or rule).
    pub importable: bool,
}

#[derive(Debug, Clone)]
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, k: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Prop> {
    // name *(";" param) ":" value — a colon inside a quoted parameter does not end the name part.
    let mut in_quote = false;
    let mut split = None;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let split = split?;
    let (head, value) = (&line[..split], &line[split + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_string(),
    })
}

/// A small table for zone names common in Outlook exports.
fn windows_zone(name: &str) -> Option<&'static str> {
    Some(match name {
        "Turkey Standard Time" => "Europe/Istanbul",
        "GMT Standard Time" => "Europe/London",
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "Pacific Standard Time" => "America/Los_Angeles",
        "UTC" | "Coordinated Universal Time" => "UTC",
        _ => return None,
    })
}

enum Value3 {
    Date(NaiveDate),
    Local(NaiveDateTime, Option<Tz>),
    Utc(NaiveDateTime),
}

fn parse_dt(p: &Prop, warnings: &mut Vec<String>) -> Option<Value3> {
    let v = p.value.trim();
    if p.param("VALUE") == Some("DATE") || (v.len() == 8 && !v.contains('T')) {
        return time::parse_key_date(v).map(Value3::Date);
    }
    if let Some(z) = v.strip_suffix('Z') {
        return time::parse_key_timed(z).map(Value3::Utc);
    }
    let local = time::parse_key_timed(v)?;
    match p.param("TZID") {
        None => Some(Value3::Local(local, None)),
        Some(name) => {
            let name = name.trim_start_matches('/');
            match time::parse_tz(name)
                .ok()
                .or_else(|| windows_zone(name).and_then(|z| time::parse_tz(z).ok()))
            {
                Some(tz) => Some(Value3::Local(local, Some(tz))),
                None => {
                    warnings.push(format!("time zone {name:?} is not supported"));
                    None
                }
            }
        }
    }
}

/// Parse a calendar file into import candidates. Never fetches anything, never trusts organizer
/// or attendee data (it is dropped), never keeps alarms.
pub(crate) fn parse_import(
    bytes: &[u8],
    default_tz: Tz,
) -> Result<(Vec<Imported>, Vec<String>), String> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "the file is larger than {} KiB",
            MAX_IMPORT_BYTES / 1024
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text".to_string())?;
    if !text.trim_start().starts_with("BEGIN:VCALENDAR") {
        return Err("not an iCalendar file".into());
    }
    // Unfold.
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if (raw.starts_with(' ') || raw.starts_with('\t')) && !lines.is_empty() {
            let last = lines.last_mut().expect("non-empty");
            if last.len() + raw.len() > MAX_LINE {
                return Err("a line is too long".into());
            }
            last.push_str(&raw[1..]);
        } else if !raw.is_empty() {
            if lines.len() >= MAX_LINES {
                return Err("the file has too many lines".into());
            }
            lines.push(raw.to_string());
        }
    }
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current: Option<Vec<Prop>> = None;
    let mut dropped: HashSet<&'static str> = HashSet::new();
    for line in &lines {
        let Some(p) = parse_line(line) else { continue };
        if p.value.len() > MAX_VALUE {
            return Err("a value is too long".into());
        }
        match p.name.as_str() {
            "BEGIN" => {
                let comp = p.value.trim().to_ascii_uppercase();
                if stack.len() >= 4 {
                    return Err("components are nested too deeply".into());
                }
                if comp == "VEVENT" && stack.last().map(String::as_str) == Some("VCALENDAR") {
                    if out.len() >= MAX_IMPORT_EVENTS {
                        return Err(format!("at most {MAX_IMPORT_EVENTS} events per file"));
                    }
                    current = Some(Vec::new());
                }
                if comp == "VALARM" {
                    dropped.insert("alarms");
                }
                stack.push(comp);
            }
            "END" => {
                let comp = stack.pop().unwrap_or_default();
                if comp == "VEVENT" {
                    if let Some(props) = current.take() {
                        out.push(build(out.len(), &props, default_tz));
                    }
                }
            }
            _ => {
                if stack.last().map(String::as_str) == Some("VEVENT") {
                    if let Some(props) = current.as_mut() {
                        match p.name.as_str() {
                            "ATTENDEE" => {
                                dropped.insert("attendees");
                            }
                            "ORGANIZER" => {
                                dropped.insert("organizers");
                            }
                            "ATTACH" => {
                                dropped.insert("attachments");
                            }
                            "RECURRENCE-ID" => {
                                dropped.insert("changed single occurrences");
                                props.push(p);
                            }
                            _ => props.push(p),
                        }
                    }
                }
            }
        }
    }
    // A VEVENT with RECURRENCE-ID changes another one; v1 imports series as authored.
    out.retain(|e: &Imported| !e.warnings.iter().any(|w| w == "__exception"));
    for (i, e) in out.iter_mut().enumerate() {
        e.index = i;
    }
    let mut d: Vec<&str> = dropped.into_iter().collect();
    d.sort();
    if !d.is_empty() {
        warnings.push(format!("not imported: {}", d.join(", ")));
    }
    Ok((out, warnings))
}

fn build(index: usize, props: &[Prop], default_tz: Tz) -> Imported {
    let mut e = Imported {
        index,
        importable: true,
        ..Imported::default()
    };
    let get = |n: &str| props.iter().find(|p| p.name == n);
    if get("RECURRENCE-ID").is_some() {
        e.warnings.push("__exception".into());
        return e;
    }
    e.title = clean_text(
        &unescape(get("SUMMARY").map(|p| p.value.as_str()).unwrap_or("")),
        200,
        false,
    );
    if e.title.is_empty() {
        e.title = "Untitled".into();
    }
    e.description = clean_text(
        &unescape(get("DESCRIPTION").map(|p| p.value.as_str()).unwrap_or("")),
        8000,
        true,
    );
    e.location = clean_text(
        &unescape(get("LOCATION").map(|p| p.value.as_str()).unwrap_or("")),
        500,
        false,
    );
    e.uid = get("UID").map(|p| clean_text(&p.value, 255, false));
    let mut w = Vec::new();
    let start = get("DTSTART").and_then(|p| parse_dt(p, &mut w));
    let end = get("DTEND").and_then(|p| parse_dt(p, &mut w));
    let duration = get("DURATION").and_then(|p| parse_duration(&p.value));
    match start {
        None => {
            e.importable = false;
            w.push("no usable start time".into());
        }
        Some(Value3::Date(d)) => {
            e.all_day = true;
            let end = match (end, duration) {
                (Some(Value3::Date(x)), _) if x > d => x,
                (_, Some(dur)) if dur.num_days() >= 1 => d + Duration::days(dur.num_days()),
                _ => d + Duration::days(1),
            };
            e.start_date = Some(time::fmt_date(d));
            e.end_date = Some(time::fmt_date(end));
        }
        Some(s) => {
            let (local, tz) = match s {
                Value3::Utc(u) => {
                    let t = u.and_utc().timestamp();
                    (time::to_local(t, default_tz), default_tz)
                }
                Value3::Local(l, Some(tz)) => (l, tz),
                Value3::Local(l, None) => {
                    w.push("floating time read in your time zone".into());
                    (l, default_tz)
                }
                Value3::Date(_) => unreachable!(),
            };
            let local_end = match (end, duration) {
                (Some(Value3::Utc(u)), _) => Some(time::to_local(u.and_utc().timestamp(), tz)),
                (Some(Value3::Local(l, Some(etz))), _) => {
                    Some(time::to_local(time::resolve_rfc(l, etz), tz))
                }
                (Some(Value3::Local(l, None)), _) => Some(l),
                (_, Some(dur)) => Some(local + dur),
                _ => None,
            }
            .filter(|x| *x > local)
            .unwrap_or(local + Duration::minutes(30));
            e.start = Some(time::fmt_local(local));
            e.end = Some(time::fmt_local(local_end));
            e.tzid = Some(tz.name().to_string());
        }
    }
    if let Some(r) = get("RRULE") {
        let start_naive = if e.all_day {
            e.start_date
                .as_deref()
                .and_then(|d| time::parse_date(d).ok())
                .map(time::midnight)
        } else {
            e.start.as_deref().and_then(|s| time::parse_local(s).ok())
        };
        match start_naive.map(|s| time::parse_rule(&r.value, e.all_day, s)) {
            Some(Ok(rule)) => e.rrule = Some(rule.body()),
            Some(Err(why)) => {
                e.importable = false;
                w.push(format!("repeat rule not supported: {why}"));
            }
            None => {}
        }
        for x in props.iter().filter(|p| p.name == "EXDATE") {
            for v in x.value.split(',') {
                let one = Prop {
                    value: v.to_string(),
                    ..x.clone()
                };
                match parse_dt(&one, &mut w) {
                    Some(Value3::Date(d)) if e.all_day => e.exdates.push(time::key_date(d)),
                    Some(Value3::Local(l, _)) if !e.all_day => e.exdates.push(time::key_timed(l)),
                    Some(Value3::Utc(u)) if !e.all_day => {
                        let tz = e
                            .tzid
                            .as_deref()
                            .and_then(|t| time::parse_tz(t).ok())
                            .unwrap_or(default_tz);
                        e.exdates
                            .push(time::key_timed(time::to_local(u.and_utc().timestamp(), tz)));
                    }
                    _ => {}
                }
            }
        }
    }
    if get("RDATE").is_some() {
        w.push("extra dates (RDATE) are not imported".into());
    }
    e.warnings = w;
    e
}

/// `PnDTnHnMnS` / `PnW`.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for ch in s.chars() {
        match ch {
            'T' => in_time = true,
            '0'..='9' => num.push(ch),
            u => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (u, in_time) {
                    ('W', false) => n * 7 * 86_400,
                    ('D', false) => n * 86_400,
                    ('H', true) => n * 3600,
                    ('M', true) => n * 60,
                    ('S', true) => n,
                    _ => return None,
                };
            }
        }
    }
    if neg || total <= 0 || total > 366 * 86_400 {
        return None;
    }
    Some(Duration::seconds(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_and_lines_fold_on_character_boundaries() {
        assert_eq!(esc("a;b,c\\d\nе"), "a\\;b\\,c\\\\d\\nе");
        let mut out = String::new();
        fold(&format!("SUMMARY:{}", "ğ".repeat(60)), &mut out);
        for l in out.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(l.len() <= 75, "{} octets", l.len());
        }
        assert!(String::from_utf8(out.into_bytes()).is_ok());
    }

    #[test]
    fn import_drops_people_alarms_and_attachments() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x@y\r\nSUMMARY:Lunch\\, team\r\n\
DTSTART;TZID=Europe/Istanbul:20261022T120000\r\nDTEND;TZID=Europe/Istanbul:20261022T130000\r\n\
RRULE:FREQ=WEEKLY;COUNT=3\r\nORGANIZER:mailto:boss@example.com\r\nATTENDEE:mailto:a@example.com\r\n\
ATTACH:https://evil.example/x\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (events, warnings) = parse_import(ics.as_bytes(), Tz::UTC).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.title, "Lunch, team");
        assert_eq!(e.start.as_deref(), Some("2026-10-22T12:00"));
        assert_eq!(e.tzid.as_deref(), Some("Europe/Istanbul"));
        assert_eq!(e.rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=3"));
        assert!(
            warnings[0].contains("alarms")
                && warnings[0].contains("attendees")
                && warnings[0].contains("attachments")
        );
    }

    #[test]
    fn import_refuses_oversized_and_deeply_nested_input() {
        assert!(parse_import(&vec![b'a'; MAX_IMPORT_BYTES + 1], Tz::UTC).is_err());
        let deep = "BEGIN:VCALENDAR\nBEGIN:A\nBEGIN:B\nBEGIN:C\nBEGIN:D\nEND:D\nEND:C\nEND:B\nEND:A\nEND:VCALENDAR\n";
        assert!(parse_import(deep.as_bytes(), Tz::UTC).is_err());
        assert!(parse_import(b"hello", Tz::UTC).is_err());
    }

    #[test]
    fn unsupported_zone_or_rule_is_marked_not_importable() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:x\nDTSTART;TZID=Mars/Olympus:20261022T120000\nEND:VEVENT\n\
BEGIN:VEVENT\nSUMMARY:y\nDTSTART:20261022T120000Z\nRRULE:FREQ=MINUTELY\nEND:VEVENT\nEND:VCALENDAR\n";
        let (events, _) = parse_import(ics.as_bytes(), Tz::UTC).unwrap();
        assert!(!events[0].importable);
        assert!(!events[1].importable);
    }

    fn sample(all_day: bool, rrule: Option<&str>) -> Ev {
        let (ls, le) = (
            time::parse_local("2026-10-22T09:00").ok(),
            time::parse_local("2026-10-22T10:00").ok(),
        );
        Ev {
            id: "evt_aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            calendar_id: "cal_x".into(),
            organizer: "/k/org".into(),
            revision: 3,
            sequence: 2,
            title: "Design review; Q4, plan".into(),
            description: "Agenda:\n1. ğüşiöç".into(),
            location: "Room 2".into(),
            all_day,
            start_date: all_day.then(|| time::parse_date("2026-10-22").unwrap()),
            end_date: all_day.then(|| time::parse_date("2026-10-24").unwrap()),
            local_start: if all_day { None } else { ls },
            local_end: if all_day { None } else { le },
            tzid: if all_day {
                None
            } else {
                Some("Europe/Berlin".into())
            },
            starts_at: None,
            ends_at: None,
            rrule: rrule.map(str::to_string),
            exdates: Vec::new(),
            transparency: "opaque".into(),
            privacy: "default".into(),
            meeting_id: Some("mtg_x".into()),
            meeting_code: Some("ABCDEFGHJK".into()),
            meeting: None,
            policy_version: 1,
            transport: "mesh".into(),
            participant_limit: 6,
            linked_group: None,
            locale: "en".into(),
            status: "confirmed".into(),
            created_by_account: None,
            split_from: None,
            span_from: 1_792_659_600,
            span_to: Some(1_800_000_000),
        }
    }

    /// Structural rules Google, Apple and Outlook import depend on; with `CALENDAR_ICS_DUMP` set,
    /// also writes the files so a second parser (Python `icalendar`) can read them back.
    #[test]
    fn written_files_follow_the_import_rules_of_common_clients() {
        let me = Recipient {
            name: "Guest".into(),
            uri: "mailto:guest@example.com".into(),
            partstat: "NEEDS-ACTION".into(),
        };
        let series = sample(false, Some("FREQ=WEEKLY;COUNT=10"));
        let moved = {
            let s = series.series();
            let inst = s.instance_at(time::parse_local("2026-11-05T09:00").unwrap());
            let mut o = effective(&series, &inst, None);
            o.local_start = time::parse_local("2026-11-05T11:00").unwrap();
            o.local_end = time::parse_local("2026-11-05T12:00").unwrap();
            o.sequence = 3;
            o
        };
        let cases: Vec<(&str, String)> = vec![
            (
                "request-series",
                render(&Doc {
                    ev: &series,
                    method: "REQUEST",
                    recipient: Some(Recipient { ..me_clone(&me) }),
                    only: None,
                    overrides: vec![moved.clone()],
                    organizer_name: "/bekir".into(),
                    inbox: "https://inbox.test",
                    busy_only: false,
                    no_description: false,
                    now: 1_792_600_000,
                }),
            ),
            (
                "cancel-one",
                render(&Doc {
                    ev: &series,
                    method: "CANCEL",
                    recipient: Some(me_clone(&me)),
                    only: Some(&Occ {
                        status: "cancelled".into(),
                        ..moved.clone()
                    }),
                    overrides: vec![],
                    organizer_name: "/bekir".into(),
                    inbox: "https://inbox.test",
                    busy_only: false,
                    no_description: false,
                    now: 1_792_600_000,
                }),
            ),
            (
                "all-day",
                render(&Doc {
                    ev: &sample(true, None),
                    method: "PUBLISH",
                    recipient: None,
                    only: None,
                    overrides: vec![],
                    organizer_name: "/bekir".into(),
                    inbox: "https://inbox.test",
                    busy_only: false,
                    no_description: false,
                    now: 1_792_600_000,
                }),
            ),
        ];
        for (name, text) in &cases {
            assert!(text.ends_with("END:VCALENDAR\r\n"), "{name}");
            for line in text.split("\r\n") {
                assert!(line.len() <= 75, "{name}: {line}");
                assert!(!line.contains('\n'));
            }
            let unfolded = text.replace("\r\n ", "");
            assert!(unfolded.contains("UID:evt_aaaaaaaaaaaaaaaaaaaaaaaaaa@pigeonpost.dev"));
            assert!(unfolded.contains("DTSTAMP:2026"));
            for tz in unfolded
                .lines()
                .filter_map(|l| l.split("TZID=").nth(1))
                .map(|r| r.split(':').next().unwrap())
            {
                assert!(
                    unfolded.contains(&format!("TZID:{tz}")),
                    "{name}: no VTIMEZONE for {tz}"
                );
            }
            assert!(!unfolded.contains("#t="));
            if let Ok(dir) = std::env::var("CALENDAR_ICS_DUMP") {
                std::fs::write(format!("{dir}/{name}.ics"), text).unwrap();
            }
        }
        let u = cases[0].1.replace("\r\n ", "");
        assert!(
            u.contains("RRULE:FREQ=WEEKLY;COUNT=10")
                && u.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261105T090000")
        );
        assert!(u.contains("SUMMARY:Design review\\; Q4\\, plan"));
        assert_eq!(u.matches("ATTENDEE").count(), 2); // one per VEVENT, always the recipient's own
        let c = cases[1].1.replace("\r\n ", "");
        assert!(
            c.contains("METHOD:CANCEL")
                && c.contains("STATUS:CANCELLED")
                && c.contains("RECURRENCE-ID")
        );
        let a = cases[2].1.replace("\r\n ", "");
        assert!(
            a.contains("DTSTART;VALUE=DATE:20261022")
                && a.contains("DTEND;VALUE=DATE:20261024")
                && !a.contains("VTIMEZONE")
        );
    }

    fn me_clone(r: &Recipient) -> Recipient {
        Recipient {
            name: r.name.clone(),
            uri: r.uri.clone(),
            partstat: r.partstat.clone(),
        }
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1W"), Some(Duration::days(7)));
        assert_eq!(parse_duration("-PT1H"), None);
    }
}
