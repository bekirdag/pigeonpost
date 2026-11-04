//! `pigeonpost calendar …` — a hosted mailbox's calendar (SDS §14B.13, `docs/calendar.md`).
//!
//! Acts with the mailbox's capability token, i.e. as an agent. The postbox decides what that may
//! do: an agent can list its occurrences, create events that invite Pigeonpost handles only, and
//! answer its own invitations. Inviting a contact (email) address sends mail, which needs a person
//! signed in with a verified address, so the postbox answers `human_required` and this command
//! says so instead of retrying.

use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};

use crate::group_cmd::{ApiError, Http, Transport};
use crate::postbox_cmd;

type Error = Box<dyn std::error::Error>;

/// Longest window the postbox lists in one call (62 days).
const MAX_DAYS: u64 = 62;

#[derive(Args)]
pub struct CalendarArgs {
    /// Which hosted mailbox to act as — required when this home holds more than one.
    #[arg(long = "as", global = true)]
    pub address: Option<String>,
    #[command(subcommand)]
    pub action: CalendarAction,
}

#[derive(Subcommand)]
pub enum CalendarAction {
    /// Occurrences from now on (events this mailbox organizes or accepted). Prints only.
    List {
        /// How many days ahead, 1–62.
        #[arg(long, default_value_t = 14)]
        days: u64,
        /// Also show invitations not answered yet.
        #[arg(long)]
        invitations: bool,
    },
    /// Create an event that invites Pigeonpost handles.
    Create {
        /// Title, 1–200 characters.
        title: String,
        /// Wall time like 2026-11-05T09:00 (or a date like 2026-11-05 with --all-day).
        #[arg(long)]
        start: String,
        /// Wall time (or exclusive end date with --all-day). Defaults to 30 minutes / one day.
        #[arg(long)]
        end: Option<String>,
        /// IANA time zone of --start/--end, e.g. Europe/Istanbul. Required for timed events.
        #[arg(long)]
        tz: Option<String>,
        #[arg(long)]
        all_day: bool,
        /// Handles to invite, comma-separated or repeated (e.g. /acme/john).
        #[arg(long, value_delimiter = ',')]
        invite: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        location: Option<String>,
        /// Repeat rule, e.g. FREQ=WEEKLY;COUNT=6.
        #[arg(long)]
        rrule: Option<String>,
        /// Attach a Pigeonpost meeting link.
        #[arg(long)]
        meeting: bool,
        /// Reuse this key when retrying, so a lost reply cannot create the event twice.
        #[arg(long)]
        request_key: Option<String>,
    },
    /// Answer an invitation.
    Rsvp {
        /// Event id (`evt_…`, shown by `calendar list --invitations`).
        event: String,
        /// accept, tentative or decline.
        #[arg(value_parser = ["accept", "accepted", "tentative", "maybe", "decline", "declined"])]
        answer: String,
        /// Answer for this one occurrence of a repeating event only (`occ_…`).
        #[arg(long)]
        occurrence: Option<String>,
        /// A short note for the organizer.
        #[arg(long)]
        note: Option<String>,
    },
}

pub async fn run(home: &std::path::Path, args: &CalendarArgs, json_out: bool) -> Result<(), Error> {
    let credential = postbox_cmd::credential_for(home, args.address.as_deref())?;
    let http = Http::new(&credential)?;
    let result = match &args.action {
        CalendarAction::List { days, invitations } => {
            let path = list_path(now(), *days, *invitations)?;
            http.call("GET", &path, None, None).await
        }
        CalendarAction::Create {
            title,
            start,
            end,
            tz,
            all_day,
            invite,
            description,
            location,
            rrule,
            meeting,
            request_key,
        } => {
            let body = create_body(&CreateInput {
                title,
                start,
                end: end.as_deref(),
                tz: tz.as_deref(),
                all_day: *all_day,
                invite,
                description: description.as_deref(),
                location: location.as_deref(),
                rrule: rrule.as_deref(),
                meeting: *meeting,
                request_key: request_key.clone().unwrap_or_else(random_key),
            })?;
            http.call("POST", "/v1/calendar/events", None, Some(body))
                .await
        }
        CalendarAction::Rsvp {
            event,
            answer,
            occurrence,
            note,
        } => {
            if !valid_id(event, "evt_") {
                return Err(format!("{event:?} is not an event id (evt_…)").into());
            }
            let body = rsvp_body(answer, occurrence.as_deref(), note.as_deref())?;
            http.call(
                "POST",
                &format!("/v1/calendar/events/{event}/rsvp"),
                None,
                Some(body),
            )
            .await
        }
    };
    let value = result.map_err(explain)?;
    if json_out {
        println!("{value}");
        return Ok(());
    }
    match &args.action {
        CalendarAction::List { .. } => print_list(&value),
        CalendarAction::Create { .. } => {
            let ev = &value["event"];
            println!(
                "created {}  {}",
                ev["id"].as_str().unwrap_or("?"),
                clean(ev["title"].as_str().unwrap_or(""))
            );
            if let Some(url) = ev["meeting"]["url"].as_str() {
                println!("  meeting  {}", clean(url));
            }
        }
        CalendarAction::Rsvp { .. } => println!(
            "answered {} for {}",
            value["rsvp"].as_str().unwrap_or("?"),
            value["event_id"].as_str().unwrap_or("?")
        ),
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_key() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut bytes);
    format!(
        "cli-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// `GET /v1/calendar/events` for `days` from `now`.
pub(crate) fn list_path(now: u64, days: u64, invitations: bool) -> Result<String, Error> {
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(format!("--days must be 1–{MAX_DAYS}").into());
    }
    let to = now + days * 86_400;
    let mut path = format!("/v1/calendar/events?from={now}&to={to}");
    if invitations {
        path.push_str("&include=invitations");
    }
    Ok(path)
}

pub(crate) struct CreateInput<'a> {
    pub title: &'a str,
    pub start: &'a str,
    pub end: Option<&'a str>,
    pub tz: Option<&'a str>,
    pub all_day: bool,
    pub invite: &'a [String],
    pub description: Option<&'a str>,
    pub location: Option<&'a str>,
    pub rrule: Option<&'a str>,
    pub meeting: bool,
    pub request_key: String,
}

/// The `POST /v1/calendar/events` body. Refuses non-handle invitees here, because from a
/// mailbox token the postbox would refuse them anyway (`human_required`).
pub(crate) fn create_body(input: &CreateInput) -> Result<Value, Error> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err("a title is required".into());
    }
    let mut body = Map::new();
    body.insert("request_key".into(), json!(input.request_key));
    body.insert("title".into(), json!(title));
    if input.all_day {
        if input.meeting {
            return Err("an all-day event cannot have a meeting link".into());
        }
        body.insert("all_day".into(), json!(true));
        body.insert("start_date".into(), json!(input.start));
        if let Some(end) = input.end {
            body.insert("end_date".into(), json!(end));
        }
    } else {
        let tz = input
            .tz
            .ok_or("--tz is required for a timed event (e.g. --tz Europe/Istanbul)")?;
        body.insert("tzid".into(), json!(tz));
        body.insert("start".into(), json!(input.start));
        if let Some(end) = input.end {
            body.insert("end".into(), json!(end));
        }
    }
    let mut attendees = Vec::new();
    for who in input.invite {
        let who = who.trim();
        if who.is_empty() {
            continue;
        }
        if !who.starts_with('/') {
            return Err(format!(
                "{who:?} is not a Pigeonpost handle. Inviting an email address sends mail, which \
                 only a person signed in to the inbox can do; agents invite handles like /acme/john."
            )
            .into());
        }
        attendees.push(json!({ "handle": who }));
    }
    if !attendees.is_empty() {
        body.insert("attendees".into(), Value::Array(attendees));
    }
    for (key, value) in [
        ("description", input.description),
        ("location", input.location),
        ("rrule", input.rrule),
    ] {
        if let Some(v) = value {
            body.insert(key.into(), json!(v));
        }
    }
    if input.meeting {
        body.insert("meeting".into(), json!({ "enabled": true }));
    }
    Ok(Value::Object(body))
}

/// The `POST /v1/calendar/events/{id}/rsvp` body.
pub(crate) fn rsvp_body(
    answer: &str,
    occurrence: Option<&str>,
    note: Option<&str>,
) -> Result<Value, Error> {
    let status = match answer {
        "accept" | "accepted" => "accepted",
        "tentative" | "maybe" => "tentative",
        "decline" | "declined" => "declined",
        other => return Err(format!("{other:?}: answer accept, tentative or decline").into()),
    };
    let mut body = json!({ "status": status });
    if let Some(occ) = occurrence {
        if !valid_id(occ, "occ_") {
            return Err(format!("{occ:?} is not an occurrence id (occ_…)").into());
        }
        body["scope"] = json!("this");
        body["occurrence_id"] = json!(occ);
    }
    if let Some(note) = note {
        body["note"] = json!(note);
    }
    Ok(body)
}

/// Ids go into a URL path, so only the postbox's own alphabet is accepted.
fn valid_id(id: &str, prefix: &str) -> bool {
    id.starts_with(prefix)
        && id.len() <= 120
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn explain(e: ApiError) -> Error {
    match e.code.as_str() {
        "human_required" => format!(
            "this needs a person signed in to Pigeonpost (human_required): {}. Agents may invite \
             handles and answer their own invitations; email guests are added by a person in the inbox.",
            e.message
        )
        .into(),
        "feature_disabled" => "the calendar is not turned on for this account".into(),
        _ => e.to_string().into(),
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// One line per occurrence. Titles come from other people: control characters are neutralised.
pub(crate) fn render_list(value: &Value) -> Vec<String> {
    let occs = value["occurrences"].as_array().cloned().unwrap_or_default();
    if occs.is_empty() {
        return vec!["nothing on the calendar in this window".into()];
    }
    occs.iter()
        .map(|o| {
            let when = if o["all_day"].as_bool() == Some(true) {
                format!("{} (all day)", o["start_date"].as_str().unwrap_or("?"))
            } else {
                format!(
                    "{} – {} {}",
                    o["start"].as_str().unwrap_or("?"),
                    o["end"]
                        .as_str()
                        .and_then(|e| e.split_once('T').map(|(_, t)| t))
                        .unwrap_or("?"),
                    o["tzid"].as_str().unwrap_or("")
                )
            };
            let answer = match o["my_rsvp"].as_str() {
                Some("needs_action") => "  [not answered]".to_string(),
                Some(r) => format!("  [{r}]"),
                None => "  [organizer]".to_string(),
            };
            let meeting = o["meeting"]["url"]
                .as_str()
                .map(|u| format!("  {u}"))
                .unwrap_or_default();
            clean(&format!(
                "{}  {when}  {}{answer}{meeting}{}",
                o["event_id"].as_str().unwrap_or("?"),
                o["title"].as_str().unwrap_or(""),
                if o["recurring"].as_bool() == Some(true) {
                    format!("  ({})", o["occurrence_id"].as_str().unwrap_or("?"))
                } else {
                    String::new()
                }
            ))
        })
        .collect()
}

fn print_list(value: &Value) {
    for line in render_list(value) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(invite: &'a [String]) -> CreateInput<'a> {
        CreateInput {
            title: " Sync ",
            start: "2026-11-05T09:00",
            end: None,
            tz: Some("UTC"),
            all_day: false,
            invite,
            description: None,
            location: None,
            rrule: None,
            meeting: true,
            request_key: "k1".into(),
        }
    }

    #[test]
    fn create_body_invites_handles_only() {
        let invite = vec!["/acme/john".to_string(), " ".to_string()];
        let body = create_body(&input(&invite)).unwrap();
        assert_eq!(body["title"], "Sync");
        assert_eq!(body["tzid"], "UTC");
        assert_eq!(body["attendees"], json!([{ "handle": "/acme/john" }]));
        assert_eq!(body["meeting"], json!({ "enabled": true }));
        assert_eq!(body["request_key"], "k1");

        let email = vec!["x@example.com".to_string()];
        let err = create_body(&input(&email)).unwrap_err().to_string();
        assert!(err.contains("only a person"), "{err}");
    }

    #[test]
    fn create_body_needs_a_zone_and_no_all_day_meeting() {
        let none: Vec<String> = Vec::new();
        let mut timed = input(&none);
        timed.tz = None;
        assert!(create_body(&timed).is_err());
        let mut day = input(&none);
        day.all_day = true;
        assert!(create_body(&day).is_err());
        day.meeting = false;
        day.start = "2026-11-05";
        let body = create_body(&day).unwrap();
        assert_eq!(body["start_date"], "2026-11-05");
        assert!(body.get("tzid").is_none() && body.get("start").is_none());
    }

    #[test]
    fn rsvp_body_maps_answers_and_scopes_occurrences() {
        assert_eq!(
            rsvp_body("accept", None, None).unwrap(),
            json!({ "status": "accepted" })
        );
        let one = rsvp_body("maybe", Some("occ_abc_20261105T090000"), Some("late")).unwrap();
        assert_eq!(one["status"], "tentative");
        assert_eq!(one["scope"], "this");
        assert_eq!(one["note"], "late");
        assert!(rsvp_body("decline", Some("../x"), None).is_err());
        assert!(rsvp_body("sure", None, None).is_err());
    }

    #[test]
    fn list_path_bounds_the_window() {
        assert_eq!(
            list_path(100, 1, true).unwrap(),
            "/v1/calendar/events?from=100&to=86500&include=invitations"
        );
        assert!(list_path(0, 0, false).is_err());
        assert!(list_path(0, 63, false).is_err());
        assert!(!valid_id("evt_a/b", "evt_"));
    }

    #[test]
    fn render_list_neutralises_control_characters() {
        let v = json!({ "occurrences": [{
            "event_id": "evt_1", "title": "Hi\u{1b}[2J", "start": "2026-11-05T09:00",
            "end": "2026-11-05T09:30", "tzid": "UTC", "my_rsvp": "needs_action",
            "meeting": { "url": "https://inbox.pigeonpost.dev/m/ABC" }
        }]});
        let lines = render_list(&v);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('\u{1b}'));
        assert!(lines[0].contains("09:30 UTC") && lines[0].contains("[not answered]"));
        assert_eq!(
            render_list(&json!({ "occurrences": [] })),
            vec!["nothing on the calendar in this window"]
        );
    }
}
