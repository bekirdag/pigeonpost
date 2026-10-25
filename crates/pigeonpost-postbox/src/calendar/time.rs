//! Calendar time: authored wall times in IANA zones, RFC 5545 recurrence, and the typed
//! recurrence keys that identify occurrences (SDS §14B.4 "Time and recurrence contract").
//!
//! # How a series is expanded
//!
//! A timed series is authored as a wall time in a zone (`09:00 Europe/Istanbul`). Recurrence rules
//! are defined over that wall clock, not over UTC: a weekly 09:00 meeting in New York stays at 09:00
//! across a DST change. So the rule is expanded over **naive wall times** — the RRULE engine (the
//! `rrule` crate) is handed the local start as if it were UTC, which makes its arithmetic purely
//! calendrical — and each generated wall time is then resolved to an instant in the zone with the
//! RFC 5545 rules (§3.3.5): a wall time inside a spring-forward gap is moved forward by the gap's
//! length, and a wall time that occurs twice takes the first (earlier) offset.
//!
//! The naive wall time of an instance *before* any per-occurrence change is its recurrence key, so
//! moving an occurrence never renames it, and a time-zone database update that changes offsets
//! changes effective UTC times without changing identities.
//!
//! Interactive authoring is stricter than expansion: a start that falls in a gap, or is ambiguous
//! without an explicit fold, is refused so the user can choose (SDS: "do not silently shift").

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    TimeZone, Timelike, Utc,
};
use chrono_tz::Tz;
use std::collections::HashSet;

/// Longest window one listing may ask for (SDS §14B.4: 62 days).
pub(crate) const MAX_WINDOW_SECS: i64 = 62 * 86_400;
/// Most occurrences one listing returns before refusing with `expansion_limit`.
pub(crate) const MAX_OCCURRENCES: usize = 1_000;
/// Largest COUNT and INTERVAL accepted.
pub(crate) const MAX_COUNT: u32 = 1_000;
const MAX_INTERVAL: u32 = 1_000;
/// Earliest and latest year an event may start in. Bounds iteration from DTSTART to any window.
const MIN_YEAR: i32 = 1970;
const MAX_YEAR: i32 = 2200;
/// Longest single event (timed or all-day).
const MAX_DURATION_DAYS: i64 = 366;

/// The IANA time-zone database release this binary carries; stored with each event so a later
/// release that moves offsets can be detected.
pub(crate) fn tzdb_version() -> &'static str {
    chrono_tz::IANA_TZDB_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimeError {
    /// Not parseable / out of range. The message is safe to show.
    Invalid(String),
    /// The wall time does not exist in that zone (spring-forward gap).
    Nonexistent,
    /// The wall time happens twice; the caller must choose with `fold`.
    Ambiguous,
    /// End is not after start.
    Order,
    ZoneUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fold {
    Earlier,
    Later,
}

impl Fold {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "earlier" => Some(Fold::Earlier),
            "later" => Some(Fold::Later),
            _ => None,
        }
    }
}

pub(crate) fn parse_tz(name: &str) -> Result<Tz, TimeError> {
    if name.is_empty() || name.len() > 64 {
        return Err(TimeError::ZoneUnknown);
    }
    name.parse::<Tz>().map_err(|_| TimeError::ZoneUnknown)
}

/// `YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS` (seconds are kept but clients send whole minutes).
pub(crate) fn parse_local(s: &str) -> Result<NaiveDateTime, TimeError> {
    let bad = || TimeError::Invalid(format!("{s:?} is not a wall time like 2026-10-22T09:00"));
    let dt = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
        .map_err(|_| bad())?;
    if !(MIN_YEAR..=MAX_YEAR).contains(&dt.year()) {
        return Err(TimeError::Invalid(format!(
            "years {MIN_YEAR}–{MAX_YEAR} only"
        )));
    }
    Ok(dt)
}

pub(crate) fn parse_date(s: &str) -> Result<NaiveDate, TimeError> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| TimeError::Invalid(format!("{s:?} is not a date like 2026-10-22")))?;
    if !(MIN_YEAR..=MAX_YEAR).contains(&d.year()) {
        return Err(TimeError::Invalid(format!(
            "years {MIN_YEAR}–{MAX_YEAR} only"
        )));
    }
    Ok(d)
}

pub(crate) fn fmt_local(dt: NaiveDateTime) -> String {
    // This representation is also persisted. Keep subminute input intact while retaining the
    // established minute-only form for the common zero-second case.
    let format = if dt.second() == 0 {
        "%Y-%m-%dT%H:%M"
    } else {
        "%Y-%m-%dT%H:%M:%S"
    };
    dt.format(format).to_string()
}

pub(crate) fn fmt_date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// The recurrence key of a timed occurrence: its original local wall time.
pub(crate) fn key_timed(dt: NaiveDateTime) -> String {
    dt.format("%Y%m%dT%H%M%S").to_string()
}

/// The recurrence key of an all-day occurrence: its original date.
pub(crate) fn key_date(d: NaiveDate) -> String {
    d.format("%Y%m%d").to_string()
}

pub(crate) fn parse_key_timed(k: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(k, "%Y%m%dT%H%M%S").ok()
}

pub(crate) fn parse_key_date(k: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(k, "%Y%m%d").ok()
}

/// Resolve an authored wall time strictly: refuse gaps, and ambiguity without a fold.
pub(crate) fn resolve_strict(
    local: NaiveDateTime,
    tz: Tz,
    fold: Option<Fold>,
) -> Result<i64, TimeError> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(dt) => Ok(dt.timestamp()),
        LocalResult::None => Err(TimeError::Nonexistent),
        LocalResult::Ambiguous(a, b) => {
            let (early, late) = if a.timestamp() <= b.timestamp() {
                (a, b)
            } else {
                (b, a)
            };
            match fold {
                Some(Fold::Earlier) => Ok(early.timestamp()),
                Some(Fold::Later) => Ok(late.timestamp()),
                None => Err(TimeError::Ambiguous),
            }
        }
    }
}

/// Resolve a generated wall time with the RFC 5545 rules: a gap shifts forward by its length
/// (equivalently: interpret with the offset in force before the gap), an overlap takes the
/// earlier instant.
pub(crate) fn resolve_rfc(local: NaiveDateTime, tz: Tz) -> i64 {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(dt) => dt.timestamp(),
        LocalResult::Ambiguous(a, b) => a.timestamp().min(b.timestamp()),
        LocalResult::None => {
            // Offset just before the gap: step back until the wall time exists.
            let mut probe = local;
            for _ in 0..48 {
                probe -= Duration::minutes(30);
                if let LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) =
                    tz.from_local_datetime(&probe)
                {
                    let offset = dt.offset().fix().local_minus_utc() as i64;
                    return local.and_utc().timestamp() - offset;
                }
            }
            local.and_utc().timestamp()
        }
    }
}

/// The wall time of an instant in a zone.
pub(crate) fn to_local(ts: i64, tz: Tz) -> NaiveDateTime {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .unwrap_or_default()
        .with_timezone(&tz)
        .naive_local()
}

/// UTC offset in seconds at an instant.
pub(crate) fn offset_at(ts: i64, tz: Tz) -> i32 {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .unwrap_or_default()
        .with_timezone(&tz)
        .offset()
        .fix()
        .local_minus_utc()
}

// ---- RRULE -------------------------------------------------------------------------------------

/// A validated recurrence rule body (no `RRULE:` prefix), normalized to upper case with its parts
/// in a stable order. `UNTIL` is a UTC date-time for timed series and a DATE for all-day series, as
/// RFC 5545 requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub parts: Vec<(String, String)>,
}

const ALLOWED: &[&str] = &[
    "FREQ",
    "INTERVAL",
    "COUNT",
    "UNTIL",
    "BYDAY",
    "BYMONTHDAY",
    "BYMONTH",
    "BYSETPOS",
    "WKST",
];

impl Rule {
    pub(crate) fn get(&self, k: &str) -> Option<&str> {
        self.parts
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }
    pub(crate) fn set(&mut self, k: &str, v: Option<String>) {
        self.parts.retain(|(key, _)| key != k);
        if let Some(v) = v {
            self.parts.push((k.to_string(), v));
        }
        self.sort();
    }
    fn sort(&mut self) {
        self.parts
            .sort_by_key(|(k, _)| ALLOWED.iter().position(|a| a == k).unwrap_or(ALLOWED.len()));
    }
    pub(crate) fn body(&self) -> String {
        self.parts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(";")
    }
    pub(crate) fn count(&self) -> Option<u32> {
        self.get("COUNT").and_then(|c| c.parse().ok())
    }
}

/// Parse and validate a rule for a series starting at `start` (local wall time; midnight for
/// all-day). Refuses anything outside the supported subset with a reason.
pub(crate) fn parse_rule(body: &str, all_day: bool, start: NaiveDateTime) -> Result<Rule, String> {
    let body = body.trim();
    let body = body
        .strip_prefix("RRULE:")
        .or_else(|| body.strip_prefix("rrule:"))
        .unwrap_or(body);
    if body.is_empty() || body.len() > 300 {
        return Err("the rule is empty or too long".into());
    }
    let mut parts: Vec<(String, String)> = Vec::new();
    for part in body.split(';') {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("{part:?} is not KEY=VALUE"))?;
        let k = k.trim().to_ascii_uppercase();
        let v = v.trim().to_ascii_uppercase();
        if !ALLOWED.contains(&k.as_str()) {
            return Err(format!("{k} is not supported"));
        }
        if parts.iter().any(|(x, _)| *x == k) {
            return Err(format!("{k} appears twice"));
        }
        if v.is_empty()
            || !v
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b",+-".contains(&b))
        {
            return Err(format!("{k} has an invalid value"));
        }
        parts.push((k, v));
    }
    let mut rule = Rule { parts };
    rule.sort();
    match rule.get("FREQ") {
        Some("DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY") => {}
        Some(other) => return Err(format!("FREQ={other} is not supported")),
        None => return Err("FREQ is required".into()),
    }
    if let Some(i) = rule.get("INTERVAL") {
        let n: u32 = i.parse().map_err(|_| "INTERVAL must be a number")?;
        if !(1..=MAX_INTERVAL).contains(&n) {
            return Err(format!("INTERVAL must be 1–{MAX_INTERVAL}"));
        }
    }
    if let Some(c) = rule.get("COUNT") {
        let n: u32 = c.parse().map_err(|_| "COUNT must be a number")?;
        if !(1..=MAX_COUNT).contains(&n) {
            return Err(format!("COUNT must be 1–{MAX_COUNT}"));
        }
        if rule.get("UNTIL").is_some() {
            return Err("COUNT and UNTIL cannot both be set".into());
        }
    }
    if let Some(u) = rule.get("UNTIL").map(str::to_string) {
        // Normalize: all-day series take a DATE; timed series a UTC date-time.
        let normalized = if all_day {
            let d =
                parse_key_date(&u[..u.len().min(8)]).ok_or("UNTIL must be a date like 20261231")?;
            key_date(d)
        } else if let Some(t) = u.strip_suffix('Z').and_then(parse_key_timed) {
            format!("{}Z", key_timed(t))
        } else if let Some(d) = parse_key_date(&u) {
            // A bare date on a timed series: through the end of that day, UTC.
            format!(
                "{}Z",
                key_timed(d.and_hms_opt(23, 59, 59).expect("valid time"))
            )
        } else {
            return Err("UNTIL must be a UTC time like 20261231T235959Z".into());
        };
        rule.set("UNTIL", Some(normalized));
    }
    // Let the engine judge the full combination (e.g. BYSETPOS without another BY part).
    let probe = naive_set(&rule, all_day, start, Tz::UTC).map_err(|e| e.to_string())?;
    let _ = probe.all(1);
    Ok(rule)
}

/// Build the engine's rule set over naive wall times (see the module docs).
fn naive_set(
    rule: &Rule,
    all_day: bool,
    start: NaiveDateTime,
    tz: Tz,
) -> Result<rrule::RRuleSet, rrule::RRuleError> {
    let mut naive = rule.clone();
    if let Some(u) = rule.get("UNTIL") {
        let local_until = if all_day {
            let d = parse_key_date(u).unwrap_or(NaiveDate::MAX);
            d.and_hms_opt(23, 59, 59).unwrap_or(start)
        } else {
            let t = u
                .strip_suffix('Z')
                .and_then(parse_key_timed)
                .unwrap_or(start);
            to_local(t.and_utc().timestamp(), tz)
        };
        naive.set("UNTIL", Some(format!("{}Z", key_timed(local_until))));
    }
    format!("DTSTART:{}Z\nRRULE:{}", key_timed(start), naive.body()).parse()
}

/// Naive wall time as the engine's UTC value, and back.
fn as_engine(dt: NaiveDateTime) -> DateTime<rrule::Tz> {
    rrule::Tz::UTC.from_utc_datetime(&dt)
}

// ---- series and instances ----------------------------------------------------------------------

/// What expansion needs to know about an event.
#[derive(Debug, Clone)]
pub(crate) struct Series {
    pub all_day: bool,
    /// Local wall start (all-day: midnight of the start date).
    pub start: NaiveDateTime,
    /// Wall-clock length (all-day: whole days).
    pub length: Duration,
    pub tz: Tz,
    pub rule: Option<Rule>,
    pub exdates: HashSet<String>,
}

/// One generated occurrence (before overrides).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Instance {
    pub key: String,
    pub local_start: NaiveDateTime,
    pub local_end: NaiveDateTime,
    /// UTC seconds; for all-day instances, UTC midnight of the dates (used only for ordering).
    pub starts_at: i64,
    pub ends_at: i64,
}

impl Instance {
    pub(crate) fn start_date(&self) -> NaiveDate {
        self.local_start.date()
    }
    #[allow(dead_code)]
    pub(crate) fn end_date(&self) -> NaiveDate {
        self.local_end.date()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExpandError {
    /// More than the caller's limit.
    Limit,
    /// The stored rule no longer parses (should not happen: rules are validated on write).
    Invalid(String),
}

impl Series {
    pub(crate) fn key_of(&self, local_start: NaiveDateTime) -> String {
        if self.all_day {
            key_date(local_start.date())
        } else {
            key_timed(local_start)
        }
    }

    pub(crate) fn instance_at(&self, local_start: NaiveDateTime) -> Instance {
        let local_end = local_start + self.length;
        let (starts_at, ends_at) = if self.all_day {
            (
                local_start.and_utc().timestamp(),
                local_end.and_utc().timestamp(),
            )
        } else {
            let s = resolve_rfc(local_start, self.tz);
            let e = resolve_rfc(local_end, self.tz).max(s + 60);
            (s, e)
        };
        Instance {
            key: self.key_of(local_start),
            local_start,
            local_end,
            starts_at,
            ends_at,
        }
    }

    /// Generated instances whose UTC span overlaps `[from, to)`, in order. All-day instances are
    /// matched with 14 hours of slack either side (their dates float with the viewer's zone).
    pub(crate) fn expand(
        &self,
        from: i64,
        to: i64,
        limit: usize,
    ) -> Result<Vec<Instance>, ExpandError> {
        let slack = if self.all_day { 14 * 3600 } else { 0 };
        let overlaps = |i: &Instance| i.starts_at < to + slack && i.ends_at > from - slack;
        let Some(rule) = &self.rule else {
            let one = self.instance_at(self.start);
            return Ok(if overlaps(&one) && !self.exdates.contains(&one.key) {
                vec![one]
            } else {
                Vec::new()
            });
        };
        // Wall-time bounds wide enough for any offset (±18 h) and the instance length.
        let lo = DateTime::<Utc>::from_timestamp(from - slack - 18 * 3600, 0)
            .unwrap_or_default()
            .naive_utc()
            - self.length;
        let hi = DateTime::<Utc>::from_timestamp(to + slack + 18 * 3600, 0)
            .unwrap_or_default()
            .naive_utc();
        let set = naive_set(rule, self.all_day, self.start, self.tz)
            .map_err(|e| ExpandError::Invalid(e.to_string()))?
            .after(as_engine(lo.max(self.start)))
            .before(as_engine(hi));
        // Engine cap well above the caller's limit; hitting it means "too many".
        let cap = (limit * 3 + 16).min(u16::MAX as usize) as u16;
        let result = set.all(cap);
        let mut out = Vec::new();
        for dt in result.dates {
            let inst = self.instance_at(dt.naive_utc());
            if self.exdates.contains(&inst.key) || !overlaps(&inst) {
                continue;
            }
            out.push(inst);
            if out.len() > limit {
                return Err(ExpandError::Limit);
            }
        }
        if result.limited && out.len() >= limit {
            return Err(ExpandError::Limit);
        }
        Ok(out)
    }

    /// Every generated instance up to `limit` (for finite series), or `None` when the series is
    /// unbounded or longer than `limit`.
    pub(crate) fn all_bounded(&self, limit: usize) -> Option<Vec<Instance>> {
        let Some(rule) = &self.rule else {
            return Some(vec![self.instance_at(self.start)]);
        };
        rule.get("COUNT").or(rule.get("UNTIL"))?;
        let set = naive_set(rule, self.all_day, self.start, self.tz).ok()?;
        let result = set.all((limit + 1).min(u16::MAX as usize) as u16);
        if result.dates.len() > limit {
            return None;
        }
        Some(
            result
                .dates
                .into_iter()
                .map(|dt| self.instance_at(dt.naive_utc()))
                .filter(|i| !self.exdates.contains(&i.key))
                .collect(),
        )
    }

    /// The first generated instance (DTSTART is always one per RFC 5545 when it matches; when it
    /// does not match the rule, the first match).
    pub(crate) fn first(&self) -> Option<Instance> {
        let Some(rule) = &self.rule else {
            return Some(self.instance_at(self.start));
        };
        let set = naive_set(rule, self.all_day, self.start, self.tz).ok()?;
        set.all(1)
            .dates
            .into_iter()
            .next()
            .map(|dt| self.instance_at(dt.naive_utc()))
    }

    /// The end of the last instance, when the series is finite and small enough to compute.
    pub(crate) fn last_end(&self) -> Option<i64> {
        self.all_bounded(MAX_COUNT as usize * 4)?
            .iter()
            .map(|i| i.ends_at)
            .max()
    }

    /// Whether `key` is one this series generates (ignoring exdates).
    pub(crate) fn generates(&self, key: &str) -> bool {
        let start = if self.all_day {
            parse_key_date(key).and_then(|d| d.and_hms_opt(0, 0, 0))
        } else {
            parse_key_timed(key)
        };
        let Some(local) = start else { return false };
        let Some(rule) = &self.rule else {
            return local == self.start;
        };
        let Ok(set) = naive_set(rule, self.all_day, self.start, self.tz) else {
            return false;
        };
        set.after(as_engine(local))
            .before(as_engine(local))
            .all(1)
            .dates
            .iter()
            .any(|d| d.naive_utc() == local)
    }

    /// Instances strictly before `key` (bounded), for splitting a series.
    pub(crate) fn count_before(
        &self,
        key_start: NaiveDateTime,
        limit: usize,
    ) -> Option<(usize, Option<Instance>)> {
        let Some(rule) = &self.rule else {
            return Some((0, None));
        };
        let set = naive_set(rule, self.all_day, self.start, self.tz).ok()?;
        let before = key_start - Duration::seconds(1);
        if before < self.start {
            return Some((0, None));
        }
        let result = set
            .before(as_engine(before))
            .all((limit + 1).min(u16::MAX as usize) as u16);
        if result.dates.len() > limit {
            return None;
        }
        let last = result.dates.last().map(|d| self.instance_at(d.naive_utc()));
        Some((result.dates.len(), last))
    }
}

/// Midnight of a date, as a wall time.
pub(crate) fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_time(NaiveTime::MIN)
}

/// Validate an authored timed range: start/end wall times in `tz`, strict resolution, positive and
/// bounded duration. Returns the UTC instants.
pub(crate) fn validate_timed(
    start: NaiveDateTime,
    end: NaiveDateTime,
    tz: Tz,
    fold: Option<Fold>,
) -> Result<(i64, i64), TimeError> {
    let s = resolve_strict(start, tz, fold)?;
    let e = resolve_strict(end, tz, fold)?;
    if e <= s || end <= start {
        return Err(TimeError::Order);
    }
    if (end - start).num_days() > MAX_DURATION_DAYS {
        return Err(TimeError::Invalid(
            "an event may last at most a year".into(),
        ));
    }
    Ok((s, e))
}

pub(crate) fn validate_dates(start: NaiveDate, end: NaiveDate) -> Result<(), TimeError> {
    if end <= start {
        return Err(TimeError::Order);
    }
    if (end - start).num_days() > MAX_DURATION_DAYS {
        return Err(TimeError::Invalid(
            "an event may last at most a year".into(),
        ));
    }
    Ok(())
}

/// Offset transitions of `tz` in `[from_year, to_year]`: `(utc instant, offset before, offset
/// after, is_dst_after)`. Used to write VTIMEZONE blocks.
pub(crate) fn transitions(tz: Tz, from_year: i32, to_year: i32) -> Vec<(i64, i32, i32, bool)> {
    use chrono_tz::OffsetComponents;
    let mut out = Vec::new();
    let Some(start) = NaiveDate::from_ymd_opt(from_year, 1, 1) else {
        return out;
    };
    let Some(end) = NaiveDate::from_ymd_opt(to_year + 1, 1, 1) else {
        return out;
    };
    let mut t = midnight(start).and_utc().timestamp();
    let stop = midnight(end).and_utc().timestamp();
    let mut prev = offset_at(t, tz);
    // Hourly scan, then bisect to the second: transitions are rare, this is cheap.
    while t < stop {
        let next = t + 3600;
        let off = offset_at(next, tz);
        if off != prev {
            let (mut lo, mut hi) = (t, next);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if offset_at(mid, tz) == prev {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let dst = DateTime::<Utc>::from_timestamp(hi, 0)
                .unwrap_or_default()
                .with_timezone(&tz)
                .offset()
                .dst_offset()
                .num_seconds()
                != 0;
            out.push((hi, prev, off, dst));
            prev = off;
        }
        t = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(s: &str) -> NaiveDateTime {
        parse_local(s).unwrap()
    }
    fn tz(s: &str) -> Tz {
        parse_tz(s).unwrap()
    }
    fn series(start: &str, mins: i64, zone: &str, rule: Option<&str>) -> Series {
        let start = local(start);
        Series {
            all_day: false,
            start,
            length: Duration::minutes(mins),
            tz: tz(zone),
            rule: rule.map(|r| parse_rule(r, false, start).unwrap()),
            exdates: HashSet::new(),
        }
    }
    fn utc(s: &str) -> i64 {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M")
            .unwrap()
            .and_utc()
            .timestamp()
    }

    #[test]
    fn istanbul_is_utc_plus_three_all_year() {
        let ist = tz("Europe/Istanbul");
        assert_eq!(
            resolve_strict(local("2026-01-15T09:00"), ist, None).unwrap(),
            utc("2026-01-15T06:00")
        );
        assert_eq!(
            resolve_strict(local("2026-07-15T09:00"), ist, None).unwrap(),
            utc("2026-07-15T06:00")
        );
        // No transitions to describe.
        assert!(transitions(ist, 2026, 2027).is_empty());
    }

    #[test]
    fn new_york_gap_and_fold_are_refused_without_a_choice() {
        let ny = tz("America/New_York");
        // 2026-03-08 02:30 does not exist; 2026-11-01 01:30 happens twice.
        assert_eq!(
            resolve_strict(local("2026-03-08T02:30"), ny, None),
            Err(TimeError::Nonexistent)
        );
        assert_eq!(
            resolve_strict(local("2026-11-01T01:30"), ny, None),
            Err(TimeError::Ambiguous)
        );
        let early = resolve_strict(local("2026-11-01T01:30"), ny, Some(Fold::Earlier)).unwrap();
        let late = resolve_strict(local("2026-11-01T01:30"), ny, Some(Fold::Later)).unwrap();
        assert_eq!(late - early, 3600);
        assert_eq!(early, utc("2026-11-01T05:30"));
    }

    #[test]
    fn rfc_resolution_shifts_gaps_forward_and_takes_the_earlier_fold() {
        let ny = tz("America/New_York");
        // 02:30 in the gap is interpreted with the pre-gap offset (EST, -5): 07:30Z = 03:30 EDT.
        assert_eq!(
            resolve_rfc(local("2026-03-08T02:30"), ny),
            utc("2026-03-08T07:30")
        );
        assert_eq!(
            resolve_rfc(local("2026-11-01T01:30"), ny),
            utc("2026-11-01T05:30")
        );
    }

    #[test]
    fn weekly_series_keeps_its_wall_time_across_dst() {
        // Every Thursday 09:00 New York, from before to after 2026-11-01.
        let s = series(
            "2026-10-22T09:00",
            60,
            "America/New_York",
            Some("FREQ=WEEKLY;COUNT=4"),
        );
        let all = s.all_bounded(10).unwrap();
        let starts: Vec<i64> = all.iter().map(|i| i.starts_at).collect();
        assert_eq!(
            starts,
            vec![
                utc("2026-10-22T13:00"), // EDT
                utc("2026-10-29T13:00"),
                utc("2026-11-05T14:00"), // EST
                utc("2026-11-12T14:00"),
            ]
        );
        assert_eq!(all[2].key, "20261105T090000");
        assert_eq!(s.last_end(), Some(utc("2026-11-12T15:00")));
    }

    #[test]
    fn daily_series_through_the_gap_moves_only_the_missing_instance() {
        let s = series(
            "2026-03-07T02:30",
            30,
            "America/New_York",
            Some("FREQ=DAILY;COUNT=3"),
        );
        let all = s.all_bounded(10).unwrap();
        assert_eq!(all[0].starts_at, utc("2026-03-07T07:30"));
        // 03-08 02:30 is in the gap: shifted forward (03:30 EDT = 07:30Z), key unchanged.
        assert_eq!(all[1].starts_at, utc("2026-03-08T07:30"));
        assert_eq!(all[1].key, "20260308T023000");
        assert_eq!(all[2].starts_at, utc("2026-03-09T06:30"));
    }

    #[test]
    fn window_expansion_includes_only_overlapping_instances_and_honours_exdates() {
        let mut s = series(
            "2026-01-01T10:00",
            60,
            "Europe/Istanbul",
            Some("FREQ=DAILY"),
        );
        s.exdates.insert("20260103T100000".into());
        let got = s
            .expand(utc("2026-01-02T00:00"), utc("2026-01-05T00:00"), 100)
            .unwrap();
        let keys: Vec<&str> = got.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, vec!["20260102T100000", "20260104T100000"]);
    }

    #[test]
    fn expansion_refuses_more_than_the_limit() {
        let s = series("2026-01-01T10:00", 30, "UTC", Some("FREQ=DAILY"));
        assert_eq!(
            s.expand(utc("2026-01-01T00:00"), utc("2026-03-01T00:00"), 10),
            Err(ExpandError::Limit)
        );
    }

    #[test]
    fn ancient_unbounded_series_reaches_today_quickly() {
        let s = series("1975-06-01T08:00", 30, "Europe/Berlin", Some("FREQ=DAILY"));
        let started = std::time::Instant::now();
        let got = s
            .expand(utc("2026-10-01T00:00"), utc("2026-10-08T00:00"), 100)
            .unwrap();
        assert_eq!(got.len(), 7);
        assert!(started.elapsed().as_secs() < 5);
    }

    #[test]
    fn unsupported_rules_are_refused_with_a_reason() {
        let start = local("2026-10-22T09:00");
        for bad in [
            "FREQ=HOURLY",
            "FREQ=SECONDLY",
            "FREQ=DAILY;BYHOUR=9",
            "FREQ=DAILY;COUNT=5;UNTIL=20261231T000000Z",
            "FREQ=DAILY;COUNT=100000",
            "FREQ=DAILY;INTERVAL=0",
            "INTERVAL=2",
            "FREQ=DAILY;FREQ=WEEKLY",
            "FREQ=WEEKLY;BYDAY=XX",
        ] {
            assert!(parse_rule(bad, false, start).is_err(), "{bad} accepted");
        }
        let ok = parse_rule("freq=weekly;byday=mo,we;until=20261231", false, start).unwrap();
        assert_eq!(ok.body(), "FREQ=WEEKLY;UNTIL=20261231T235959Z;BYDAY=MO,WE");
    }

    #[test]
    fn all_day_series_use_dates_and_exclusive_ends() {
        let start = midnight(parse_date("2026-10-30").unwrap());
        let s = Series {
            all_day: true,
            start,
            length: Duration::days(2),
            tz: Tz::UTC,
            rule: Some(parse_rule("FREQ=MONTHLY;COUNT=2", true, start).unwrap()),
            exdates: HashSet::new(),
        };
        let all = s.all_bounded(5).unwrap();
        assert_eq!(all[0].key, "20261030");
        assert_eq!(fmt_date(all[0].end_date()), "2026-11-01");
        assert_eq!(all[1].key, "20261130");
    }

    #[test]
    fn splitting_counts_the_instances_before_a_key() {
        let s = series(
            "2026-10-01T09:00",
            30,
            "Europe/Istanbul",
            Some("FREQ=DAILY;COUNT=10"),
        );
        let (n, last) = s.count_before(local("2026-10-05T09:00"), 100).unwrap();
        assert_eq!(n, 4);
        assert_eq!(last.unwrap().key, "20261004T090000");
        assert!(s.generates("20261005T090000"));
        assert!(!s.generates("20261005T100000"));
    }

    #[test]
    fn berlin_transitions_are_found_to_the_second() {
        let t = transitions(tz("Europe/Berlin"), 2026, 2026);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].0, utc("2026-03-29T01:00"));
        assert_eq!((t[0].1, t[0].2, t[0].3), (3600, 7200, true));
        assert_eq!(t[1].0, utc("2026-10-25T01:00"));
        assert!(!t[1].3);
    }
}
