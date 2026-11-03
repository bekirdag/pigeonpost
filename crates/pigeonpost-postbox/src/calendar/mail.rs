//! Messages to contact addresses (SDS §14B.6): themed templates rendered here, sent through
//! saas_be's internal notification route to the pinned Novu workflows.
//!
//! The postbox renders subject, HTML and plain text itself (EN and TR, light with a dark-mode media
//! query, the Pigeonpost brand). saas_be passes the HTML through unchanged. Every send carries the
//! durable delivery id as `transaction_id`, so a retry is recognisable downstream; exactly-once
//! delivery is not claimed.
//!
//! Configuration (all in the postbox env file, never in the repository):
//! `POSTBOX_CALENDAR_NOTIFY_URL` (the full saas_be internal notification URL),
//! `POSTBOX_CALENDAR_NOTIFY_KEY` (a platform notification key with `notifications:write`),
//! `POSTBOX_CALENDAR_FROM` (sender address; unset = the integration's default sender),
//! `POSTBOX_CALENDAR_TENANT` (default `pigeonpost`). Unset URL or key = nothing is sent; rows stay
//! queued.

use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::sync::Mutex;

/// One message to send.
#[derive(Debug, Clone)]
pub(crate) struct Outgoing {
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub workflow: &'static str,
    pub transaction_id: String,
    pub sender_name: String,
    pub payload: Value,
    /// `(filename, content type, bytes)`.
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendResult {
    /// Accepted downstream (provider reference, if any).
    Accepted(Option<String>),
    /// Refused permanently (bad request): do not retry.
    Rejected(String),
    /// Temporary failure: retry later.
    Retry(String),
    /// The request may or may not have been accepted (timeout after sending).
    Uncertain,
    /// Not configured.
    Unconfigured,
}

pub(crate) enum Notifier {
    Off,
    Http {
        url: String,
        key: String,
        from: Option<String>,
        tenant: String,
        client: reqwest::Client,
    },
    /// Tests: keep what would have been sent.
    #[allow(dead_code)]
    Capture(Mutex<Vec<Outgoing>>),
}

impl Notifier {
    pub(crate) fn from_env() -> Self {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        match (
            get("POSTBOX_CALENDAR_NOTIFY_URL"),
            get("POSTBOX_CALENDAR_NOTIFY_KEY"),
        ) {
            (Some(url), Some(key))
                if url.starts_with("https://") || url.starts_with("http://127.") =>
            {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(20))
                    .build()
                    .unwrap_or_default();
                Notifier::Http {
                    url,
                    key,
                    from: get("POSTBOX_CALENDAR_FROM"),
                    tenant: get("POSTBOX_CALENDAR_TENANT").unwrap_or_else(|| "pigeonpost".into()),
                    client,
                }
            }
            _ => Notifier::Off,
        }
    }

    #[cfg(test)]
    pub(crate) fn capture() -> Self {
        Notifier::Capture(Mutex::new(Vec::new()))
    }

    #[cfg(test)]
    pub(crate) fn sent(&self) -> Vec<Outgoing> {
        match self {
            Notifier::Capture(m) => m.lock().expect("capture lock").clone(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn configured(&self) -> bool {
        !matches!(self, Notifier::Off)
    }

    pub(crate) async fn send(&self, m: &Outgoing) -> SendResult {
        match self {
            Notifier::Off => SendResult::Unconfigured,
            Notifier::Capture(list) => {
                list.lock().expect("capture lock").push(m.clone());
                SendResult::Accepted(Some(format!("capture-{}", m.transaction_id)))
            }
            Notifier::Http {
                url,
                key,
                from,
                tenant,
                client,
            } => {
                let attachments: Vec<Value> = m
                    .attachments
                    .iter()
                    .map(|(name, ty, bytes)| {
                        json!({ "filename": name, "content_type": ty, "content": crate::b64_encode(bytes) })
                    })
                    .collect();
                let mut body = json!({
                    "tenant_id": tenant, "to": m.to, "subject": m.subject, "html": m.html,
                    "text": m.text, "workflow_id": m.workflow, "transaction_id": m.transaction_id,
                    "sender_name": m.sender_name, "payload": m.payload,
                });
                if let Some(f) = from {
                    body["from"] = json!(f);
                }
                if !attachments.is_empty() {
                    body["attachments"] = Value::Array(attachments);
                }
                let resp = client
                    .post(url)
                    .header("x-api-key", key)
                    .header("idempotency-key", &m.transaction_id)
                    .json(&body)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let v: Value = r.json().await.unwrap_or(Value::Null);
                        let reference = ["id", "message_id", "transaction_id", "notification_id"]
                            .iter()
                            .find_map(|k| v[*k].as_str().map(str::to_string));
                        SendResult::Accepted(reference.or(Some(m.transaction_id.clone())))
                    }
                    Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
                        SendResult::Retry(format!("status {}", r.status().as_u16()))
                    }
                    Ok(r) => SendResult::Rejected(format!("status {}", r.status().as_u16())),
                    Err(e) if e.is_timeout() => SendResult::Uncertain,
                    Err(e) if e.is_connect() => SendResult::Retry("connect".into()),
                    Err(_) => SendResult::Uncertain,
                }
            }
        }
    }
}

// ---- rendering ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Invite,
    Update,
    Cancel,
    Reminder,
    Verify,
}

/// Everything a template may show. All strings are plain text; rendering escapes them.
#[derive(Debug, Clone, Default)]
pub(crate) struct View {
    pub lang: String,
    pub title: String,
    pub organizer: String,
    pub when: String,
    pub location: String,
    pub description: String,
    pub meeting_url: Option<String>,
    pub page_url: String,
    pub rsvp_urls: Option<[String; 3]>,
    pub unsubscribe_organizer: Option<String>,
    pub unsubscribe_all: Option<String>,
    pub cancel_message: Option<String>,
    pub minutes: i64,
    pub code: Option<String>,
    /// The guest page with an "add to calendar" action (`…#t=<token>&a=ics`).
    pub ics_url: Option<String>,
    #[allow(dead_code)]
    pub recipient_name: String,
}

struct Words {
    invite_subject: &'static str,
    update_subject: &'static str,
    cancel_subject: &'static str,
    reminder_subject: &'static str,
    verify_subject: &'static str,
    invited: &'static str,
    updated: &'static str,
    cancelled: &'static str,
    reminder: &'static str,
    when: &'static str,
    where_: &'static str,
    join: &'static str,
    details: &'static str,
    yes: &'static str,
    maybe: &'static str,
    no: &'static str,
    going: &'static str,
    attached: &'static str,
    authoritative: &'static str,
    why: &'static str,
    invited_anon: &'static str,
    updated_anon: &'static str,
    cancelled_anon: &'static str,
    why_anon: &'static str,
    stop_from: &'static str,
    stop_all: &'static str,
    no_reply: &'static str,
    verify_intro: &'static str,
    verify_note: &'static str,
    verify_title: &'static str,
    add_to_calendar: &'static str,
    message_from: &'static str,
}

const EN: Words = Words {
    invite_subject: "Invitation: {title}",
    update_subject: "Updated: {title}",
    cancel_subject: "Cancelled: {title}",
    reminder_subject: "Starting soon: {title}",
    verify_subject: "Your Pigeonpost code: {code}",
    invited: "{organizer} invited you",
    updated: "{organizer} changed this event",
    cancelled: "{organizer} cancelled this event",
    reminder: "Starts in {minutes} minutes",
    when: "When",
    where_: "Where",
    join: "Join with Pigeonpost",
    details: "Open the invitation",
    yes: "Yes",
    maybe: "Maybe",
    no: "No",
    going: "Going?",
    attached: "A calendar file is attached so you can add this to your calendar app.",
    authoritative: "Answer with the buttons above. Answers sent from your calendar app's own buttons do not reach Pigeonpost.",
    why: "You received this because {organizer} entered this address as a guest on Pigeonpost.",
    invited_anon: "You're invited",
    updated_anon: "This event was changed",
    cancelled_anon: "This event was cancelled",
    why_anon: "You received this because the organizer entered this address as a guest on Pigeonpost.",
    stop_from: "Don't send me invitations from this person",
    stop_all: "Don't send me invitations from anyone on Pigeonpost",
    no_reply: "Replies to this message are not read.",
    verify_intro: "Enter this code on the invitation page to confirm it's you:",
    verify_note: "It works once and expires in 10 minutes. If you didn't ask for it, you can ignore this message.",
    message_from: "Message from the organizer",
    verify_title: "Confirm it's you",
    add_to_calendar: "Add to calendar",
};

const TR: Words = Words {
    invite_subject: "Davet: {title}",
    update_subject: "Güncellendi: {title}",
    cancel_subject: "İptal edildi: {title}",
    reminder_subject: "Birazdan başlıyor: {title}",
    verify_subject: "Pigeonpost kodunuz: {code}",
    invited: "{organizer} sizi davet etti",
    updated: "{organizer} bu etkinliği değiştirdi",
    cancelled: "{organizer} bu etkinliği iptal etti",
    reminder: "{minutes} dakika sonra başlıyor",
    when: "Ne zaman",
    where_: "Nerede",
    join: "Pigeonpost ile katıl",
    details: "Daveti aç",
    yes: "Evet",
    maybe: "Belki",
    no: "Hayır",
    going: "Katılacak mısınız?",
    attached: "Takvim uygulamanıza ekleyebilmeniz için bir takvim dosyası ektedir.",
    authoritative: "Yukarıdaki düğmelerle yanıt verin. Takvim uygulamanızın kendi düğmeleriyle verilen yanıtlar Pigeonpost'a ulaşmaz.",
    why: "Bu mesajı, {organizer} bu adresi Pigeonpost'ta konuk olarak girdiği için aldınız.",
    invited_anon: "Davet edildiniz",
    updated_anon: "Bu etkinlik değiştirildi",
    cancelled_anon: "Bu etkinlik iptal edildi",
    why_anon: "Bu mesajı, düzenleyen bu adresi Pigeonpost'ta konuk olarak girdiği için aldınız.",
    stop_from: "Bu kişiden davet gönderilmesin",
    stop_all: "Pigeonpost'taki hiç kimseden davet gönderilmesin",
    no_reply: "Bu mesaja verilen yanıtlar okunmaz.",
    verify_intro: "Siz olduğunuzu doğrulamak için bu kodu davet sayfasına girin:",
    verify_note: "Kod bir kez kullanılabilir ve 10 dakika içinde geçerliliğini yitirir. Siz istemediyseniz bu mesajı yok sayabilirsiniz.",
    message_from: "Düzenleyenin mesajı",
    verify_title: "Siz olduğunuzu doğrulayın",
    add_to_calendar: "Takvime ekle",
};

fn words(lang: &str) -> &'static Words {
    if lang == "tr" {
        &TR
    } else {
        &EN
    }
}

pub(crate) fn html_escape(s: &str) -> String {
    crate::mail_layout::escape(s)
}

fn fill(t: &str, v: &View) -> String {
    t.replace("{title}", &v.title)
        .replace("{organizer}", &v.organizer)
        .replace("{minutes}", &v.minutes.to_string())
        .replace("{code}", v.code.as_deref().unwrap_or(""))
}

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_TR: [&str; 12] = [
    "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim",
    "Kasım", "Aralık",
];
const DAYS_EN: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const DAYS_TR: [&str; 7] = [
    "Pazartesi",
    "Salı",
    "Çarşamba",
    "Perşembe",
    "Cuma",
    "Cumartesi",
    "Pazar",
];

/// A time line in the organizer's named zone with its UTC offset (SDS §14B.6: never the link
/// scanner's browser zone).
pub(crate) fn when_timed(lang: &str, starts_at: i64, ends_at: i64, tz: Tz) -> String {
    let s = DateTime::<Utc>::from_timestamp(starts_at, 0)
        .unwrap_or_default()
        .with_timezone(&tz);
    let e = DateTime::<Utc>::from_timestamp(ends_at, 0)
        .unwrap_or_default()
        .with_timezone(&tz);
    let off = super::time::offset_at(starts_at, tz);
    let offset = format!(
        "UTC{}{:02}:{:02}",
        if off < 0 { '-' } else { '+' },
        off.abs() / 3600,
        (off.abs() % 3600) / 60
    );
    let date = |d: &DateTime<Tz>| {
        let wd = d.weekday().num_days_from_monday() as usize;
        let m = d.month0() as usize;
        if lang == "tr" {
            format!("{} {} {} {}", d.day(), MONTHS_TR[m], d.year(), DAYS_TR[wd])
        } else {
            format!("{}, {} {} {}", DAYS_EN[wd], d.day(), MONTHS_EN[m], d.year())
        }
    };
    let same_day = s.date_naive() == e.date_naive();
    if same_day {
        format!(
            "{}, {}–{} ({}, {offset})",
            date(&s),
            s.format("%H:%M"),
            e.format("%H:%M"),
            tz.name()
        )
    } else {
        format!(
            "{}, {} – {}, {} ({}, {offset})",
            date(&s),
            s.format("%H:%M"),
            date(&e),
            e.format("%H:%M"),
            tz.name()
        )
    }
}

pub(crate) fn when_dates(
    lang: &str,
    start: chrono::NaiveDate,
    end_exclusive: chrono::NaiveDate,
) -> String {
    let fmt = |d: chrono::NaiveDate| {
        let wd = d.weekday().num_days_from_monday() as usize;
        let m = d.month0() as usize;
        if lang == "tr" {
            format!("{} {} {} {}", d.day(), MONTHS_TR[m], d.year(), DAYS_TR[wd])
        } else {
            format!("{}, {} {} {}", DAYS_EN[wd], d.day(), MONTHS_EN[m], d.year())
        }
    };
    let last = end_exclusive.pred_opt().unwrap_or(start);
    let all = if lang == "tr" { "tüm gün" } else { "all day" };
    if last == start {
        format!("{} ({all})", fmt(start))
    } else {
        format!("{} – {} ({all})", fmt(start), fmt(last))
    }
}

/// Subject, HTML and plain text for one message.
pub(crate) fn render(kind: Kind, v: &View) -> (String, String, String) {
    let w = words(&v.lang);
    let subject = fill(
        match kind {
            Kind::Invite => w.invite_subject,
            Kind::Update => w.update_subject,
            Kind::Cancel => w.cancel_subject,
            Kind::Reminder => w.reminder_subject,
            Kind::Verify => w.verify_subject,
        },
        v,
    );
    let subject: String = subject
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let lead = fill(
        match kind {
            // No name to show (a key-only organizer): never print the raw address.
            Kind::Invite if v.organizer.is_empty() => w.invited_anon,
            Kind::Update if v.organizer.is_empty() => w.updated_anon,
            Kind::Cancel if v.organizer.is_empty() => w.cancelled_anon,
            Kind::Invite => w.invited,
            Kind::Update => w.updated,
            Kind::Cancel => w.cancelled,
            Kind::Reminder => w.reminder,
            Kind::Verify => w.verify_intro,
        },
        v,
    );
    use crate::mail_layout::{self as layout, BODY, INK, MONO, MUTED, RULE, SOFT};
    let e = html_escape;
    let mut h = String::new();
    let mut t = String::new();
    let p_body = format!("margin:0 0 14px;color:{BODY};font-size:15px;line-height:1.6;");
    let p_note = format!("margin:14px 0 0;color:{MUTED};font-size:13px;line-height:1.55;");
    if kind != Kind::Verify {
        h.push_str(&format!(
            "<p class=\"pp-muted\" style=\"margin:0 0 6px;color:{MUTED};font-size:14px;line-height:1.5;\">{}</p>\n",
            e(&lead)
        ));
    }
    t.push_str(&lead);
    t.push_str("\n\n");
    let preheader = if kind == Kind::Verify {
        fill(w.verify_subject, v)
    } else {
        format!("{lead} · {}", v.when)
    };
    if kind == Kind::Verify {
        let code = v.code.clone().unwrap_or_default();
        h.push_str(&format!(
            "<h1 class=\"pp-ink\" style=\"margin:0 0 16px;font-size:20px;line-height:1.3;font-weight:700;letter-spacing:-0.01em;color:{INK};\">{}</h1>\n",
            e(w.verify_title)
        ));
        h.push_str(&format!(
            "<p class=\"pp-body\" style=\"{p_body}margin-bottom:20px;\">{}</p>\n",
            e(&lead)
        ));
        h.push_str(&format!(
            "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr><td align=\"center\" class=\"pp-code\" style=\"background-color:{SOFT};border:1px solid {RULE};border-radius:10px;padding:18px 12px 18px 22px;font-family:{MONO};font-size:40px;line-height:1.1;font-weight:700;letter-spacing:10px;color:{INK};-webkit-user-select:all;user-select:all;\">{}</td></tr></table>\n",
            e(&code)
        ));
        h.push_str(&format!(
            "<p class=\"pp-muted pp-rule\" style=\"{p_note}margin-top:20px;padding-top:16px;border-top:1px solid {RULE};\">{}</p>\n",
            e(w.verify_note)
        ));
        t.push_str(&format!("{code}\n\n{}\n", w.verify_note));
    } else {
        h.push_str(&format!(
            "<h1 class=\"pp-ink\" style=\"margin:0 0 18px;font-size:22px;line-height:1.3;font-weight:700;letter-spacing:-0.01em;color:{INK};\">{}</h1>\n",
            e(&v.title)
        ));
        t.push_str(&format!("{}\n\n", v.title));
        let label = |s: &str| {
            format!(
                "<span class=\"pp-muted\" style=\"display:block;font-size:12px;line-height:1.4;text-transform:uppercase;letter-spacing:.04em;color:{MUTED};\">{}</span>",
                e(s)
            )
        };
        h.push_str(&format!(
            "<p class=\"pp-body\" style=\"{p_body}\">{}{}</p>\n",
            label(w.when),
            e(&v.when)
        ));
        t.push_str(&format!("{}: {}\n", w.when, v.when));
        if !v.location.is_empty() {
            h.push_str(&format!(
                "<p class=\"pp-body\" style=\"{p_body}\">{}{}</p>\n",
                label(w.where_),
                e(&v.location)
            ));
            t.push_str(&format!("{}: {}\n", w.where_, v.location));
        }
        let soft = format!(
            "white-space:pre-wrap;font-size:14px;line-height:1.55;color:{BODY};background-color:{SOFT};border:1px solid {RULE};border-radius:10px;padding:12px 14px;margin:0 0 14px;"
        );
        if let Some(m) = &v.cancel_message {
            h.push_str(&format!(
                "<p class=\"pp-body\" style=\"margin:0 0 6px;\">{}</p><div class=\"pp-soft\" style=\"{soft}\">{}</div>\n",
                label(w.message_from),
                e(m)
            ));
            t.push_str(&format!("\n{}:\n{m}\n", w.message_from));
        }
        if !v.description.is_empty() && kind != Kind::Cancel {
            // Shown as text: links in descriptions are never made clickable here.
            h.push_str(&format!(
                "<div class=\"pp-soft\" style=\"{soft}\">{}</div>\n",
                e(&v.description)
            ));
            t.push_str(&format!("\n{}\n", v.description));
        }
        if kind != Kind::Cancel {
            h.push_str("<p style=\"margin:22px 0 6px;\">");
            match &v.meeting_url {
                Some(url) => {
                    h.push_str(&layout::button(url, w.join));
                    t.push_str(&format!("\n{}: {url}\n", w.join));
                }
                None => {
                    h.push_str(&layout::button(&v.page_url, w.details));
                    t.push_str(&format!("\n{}: {}\n", w.details, v.page_url));
                }
            }
            if let Some(ics) = &v.ics_url {
                h.push_str(&layout::ghost(ics, w.add_to_calendar));
                t.push_str(&format!("{}: {ics}\n", w.add_to_calendar));
            }
            h.push_str("</p>\n");
        }
        if let (Some([yes, maybe, no]), true) =
            (&v.rsvp_urls, matches!(kind, Kind::Invite | Kind::Update))
        {
            h.push_str(&format!(
                "<p class=\"pp-body\" style=\"margin:18px 0 8px;\">{}</p><p style=\"margin:0;\">",
                label(w.going)
            ));
            t.push('\n');
            for (answer, url) in [(w.yes, yes), (w.maybe, maybe), (w.no, no)] {
                h.push_str(&layout::ghost(url, answer));
                t.push_str(&format!("{answer}: {url}\n"));
            }
            h.push_str("</p>\n");
            h.push_str(&format!(
                "<p class=\"pp-muted\" style=\"{p_note}\">{}</p>\n",
                e(w.authoritative)
            ));
            t.push_str(&format!("\n{}\n", w.authoritative));
        }
        if matches!(kind, Kind::Invite | Kind::Update | Kind::Cancel) {
            h.push_str(&format!(
                "<p class=\"pp-muted pp-rule\" style=\"{p_note}margin-top:20px;padding-top:16px;border-top:1px solid {RULE};\">{}</p>\n",
                e(w.attached)
            ));
            t.push_str(&format!("{}\n", w.attached));
        }
    }
    let mut foot = String::from(FOOTER_BRAND);
    t.push_str("\n--\n");
    if kind != Kind::Verify {
        let why = fill(
            if v.organizer.is_empty() {
                w.why_anon
            } else {
                w.why
            },
            v,
        );
        foot.push_str(&format!("<br>{}<br>{}", e(&why), e(w.no_reply)));
        t.push_str(&format!("{why}\n{}\n", w.no_reply));
        if let Some(u) = &v.unsubscribe_organizer {
            let link = |href: &str, label: &str| {
                format!(
                    "<a class=\"pp-muted\" href=\"{}\" style=\"color:{MUTED};text-decoration:underline;\">{}</a>",
                    e(href),
                    e(label)
                )
            };
            foot.push_str(&format!("<br>{}", link(u, w.stop_from)));
            t.push_str(&format!("{}: {u}\n", w.stop_from));
            if let Some(a) = &v.unsubscribe_all {
                foot.push_str(&format!(" · {}", link(a, w.stop_all)));
                t.push_str(&format!("{}: {a}\n", w.stop_all));
            }
        }
    } else {
        foot.push_str(&format!("<br>{}", e(w.no_reply)));
        t.push_str(&format!("{}\n", w.no_reply));
    }
    t.push_str("Pigeonpost · pigeonpost.dev\n");
    let html = layout::document(&v.lang, &subject, &preheader, &h, &foot);
    (subject, html, t)
}

/// The first footer line, shared with the Keycloak sign-in code theme.
const FOOTER_BRAND: &str =
    "Pigeonpost · Private messaging for people and AI agents · pigeonpost.dev";

/// The `From` display name for a calendar message: "<name> via Pigeonpost" when the organizer
/// has a name (their self-set name or a name-shaped handle; see `worker.rs`), otherwise
/// "Invitation via Pigeonpost" for invitations and reminders and "Pigeonpost Calendar" for
/// updates and cancellations. Never a raw `/k/…` address. Header-safe and RFC 2047-encoded by
/// [`crate::mail_layout::sender_display`].
pub(crate) fn sender_name(organizer: Option<&str>, kind: Kind) -> String {
    let fallback = match kind {
        Kind::Update | Kind::Cancel => "Pigeonpost Calendar",
        Kind::Verify => "Pigeonpost",
        Kind::Invite | Kind::Reminder => "Invitation via Pigeonpost",
    };
    crate::mail_layout::sender_display(organizer, fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(lang: &str) -> View {
        View {
            lang: lang.into(),
            title: "Design <review> & \"plan\"".into(),
            organizer: "/bekir".into(),
            when: when_timed(
                lang,
                1_792_735_200,
                1_792_738_800,
                "Europe/Istanbul".parse().unwrap(),
            ),
            location: "Room 2".into(),
            description: "See https://evil.example/x".into(),
            meeting_url: Some("https://inbox.test/m/ABCDEFGHJK".into()),
            page_url: "https://inbox.test/m/ABCDEFGHJK#t=x".into(),
            rsvp_urls: Some(["y".into(), "m".into(), "n".into()]),
            unsubscribe_organizer: Some("https://inbox.test/m/u#u=a&s=organizer".into()),
            unsubscribe_all: Some("https://inbox.test/m/u#u=a&s=all".into()),
            ..View::default()
        }
    }

    #[test]
    fn templates_escape_and_never_link_descriptions() {
        let (subject, html, text) = render(Kind::Invite, &view("en"));
        assert_eq!(subject, "Invitation: Design <review> & \"plan\"");
        assert!(html.contains("Design &lt;review&gt; &amp; &quot;plan&quot;"));
        assert!(!html.contains("href=\"https://evil.example"));
        assert!(html.contains("prefers-color-scheme: dark"));
        assert!(text.contains("Yes: y") && text.contains("Room 2"));
        assert!(html.contains("Don&#39;t send me invitations from this person"));
    }

    #[test]
    fn turkish_template_and_time_line() {
        let (subject, html, _) = render(Kind::Cancel, &view("tr"));
        assert!(subject.starts_with("İptal edildi:"));
        assert!(html.contains("Ekim 2026"));
        assert!(html.contains("UTC+03:00"));
    }

    #[test]
    fn sender_names_cannot_inject_headers() {
        let from = sender_name(Some("Evil\r\nBcc: x@y <a>"), Kind::Invite);
        assert_eq!(from, "Evil Bcc xy a via Pigeonpost");
        assert!(!from.contains(['\r', '\n', '<', '>', '@', ':', '"']));
    }

    #[test]
    fn key_only_organizers_get_neutral_senders_and_wording() {
        assert_eq!(sender_name(None, Kind::Invite), "Invitation via Pigeonpost");
        assert_eq!(
            sender_name(None, Kind::Reminder),
            "Invitation via Pigeonpost"
        );
        assert_eq!(sender_name(None, Kind::Update), "Pigeonpost Calendar");
        assert_eq!(sender_name(None, Kind::Cancel), "Pigeonpost Calendar");
        assert_eq!(
            sender_name(Some("Bekir"), Kind::Cancel),
            "Bekir via Pigeonpost"
        );
        let mut v = view("en");
        v.organizer = String::new();
        let (_, html, text) = render(Kind::Invite, &v);
        assert!(text.starts_with("You're invited\n"));
        assert!(text.contains("because the organizer entered this address"));
        assert!(!html.contains("{organizer}") && !text.contains("{organizer}"));
        let (_, _, text) = render(Kind::Cancel, &v);
        assert!(text.starts_with("This event was cancelled\n"));
        let (_, _, text) = render(Kind::Update, &view("tr"));
        assert!(text.starts_with("/bekir bu etkinliği"));
    }

    #[test]
    fn non_ascii_names_stay_plain_and_header_safe() {
        let from = sender_name(Some("Çağrı \"Dağ\" <x@y>"), Kind::Invite);
        assert_eq!(from, "Çağrı Dağ xy via Pigeonpost");
    }
}
