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
    stop_from: &'static str,
    stop_all: &'static str,
    no_reply: &'static str,
    verify_intro: &'static str,
    verify_note: &'static str,
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
    stop_from: "Don't send me invitations from this person",
    stop_all: "Don't send me invitations from anyone on Pigeonpost",
    no_reply: "Replies to this message are not read.",
    verify_intro: "Enter this code on the invitation page to confirm it's you:",
    verify_note: "It works once and expires in 10 minutes. If you didn't ask for it, you can ignore this message.",
    message_from: "Message from the organizer",
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
    stop_from: "Bu kişiden davet gönderilmesin",
    stop_all: "Pigeonpost'taki hiç kimseden davet gönderilmesin",
    no_reply: "Bu mesaja verilen yanıtlar okunmaz.",
    verify_intro: "Siz olduğunuzu doğrulamak için bu kodu davet sayfasına girin:",
    verify_note: "Kod bir kez kullanılabilir ve 10 dakika içinde geçerliliğini yitirir. Siz istemediyseniz bu mesajı yok sayabilirsiniz.",
    message_from: "Düzenleyenin mesajı",
};

fn words(lang: &str) -> &'static Words {
    if lang == "tr" {
        &TR
    } else {
        &EN
    }
}

pub(crate) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
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
            Kind::Invite => w.invited,
            Kind::Update => w.updated,
            Kind::Cancel => w.cancelled,
            Kind::Reminder => w.reminder,
            Kind::Verify => w.verify_intro,
        },
        v,
    );
    let e = html_escape;
    let mut h = String::new();
    let mut t = String::new();
    h.push_str(&format!(
        r#"<!doctype html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><meta name="supported-color-schemes" content="light dark"><title>{subject}</title><style>
body{{margin:0;padding:0;background:#f4f6f8;color:#12161c;font-family:Geist,"Segoe UI",-apple-system,BlinkMacSystemFont,Roboto,Helvetica,Arial,sans-serif;}}
.wrap{{max-width:560px;margin:0 auto;padding:24px 16px;}}
.card{{background:#ffffff;border:1px solid #e4e7ec;border-radius:14px;padding:28px;}}
.brand{{display:flex;align-items:center;gap:10px;margin-bottom:20px;font-weight:600;font-size:15px;color:#12161c;}}
.lead{{color:#59626e;font-size:14px;margin:0 0 6px;}}
h1{{font-size:22px;line-height:1.3;margin:0 0 18px;color:#12161c;}}
.row{{margin:0 0 12px;font-size:15px;line-height:1.5;color:#343c47;}}
.label{{display:block;font-size:12px;text-transform:uppercase;letter-spacing:.04em;color:#636c78;}}
.btn{{display:inline-block;background:#2563eb;color:#ffffff !important;text-decoration:none;font-weight:600;padding:12px 20px;border-radius:10px;font-size:15px;}}
.ghost{{display:inline-block;border:1px solid #e4e7ec;color:#12161c !important;text-decoration:none;padding:9px 16px;border-radius:10px;margin:0 6px 6px 0;font-size:14px;}}
.code{{font-size:30px;letter-spacing:.3em;font-weight:700;margin:12px 0;color:#12161c;}}
.desc{{white-space:pre-wrap;font-size:14px;color:#343c47;background:#f4f6f8;border-radius:10px;padding:12px;}}
.foot{{font-size:12px;color:#636c78;line-height:1.6;margin-top:18px;}}
.foot a{{color:#636c78;}}
@media (prefers-color-scheme: dark){{
body{{background:#0f1216 !important;color:#e8ebef !important;}}
.card{{background:#171b21 !important;border-color:#2a3038 !important;}}
.brand,h1,.code{{color:#f2f4f7 !important;}}
.row,.desc{{color:#c9ced6 !important;}}
.desc{{background:#0f1216 !important;}}
.lead,.label,.foot,.foot a{{color:#9aa3ae !important;}}
.ghost{{border-color:#2a3038 !important;color:#e8ebef !important;}}
.btn{{background:#3b82f6 !important;}}
}}
</style></head><body><div class="wrap"><div class="card">
<div class="brand"><img src="https://pigeonpost.dev/logo.png" width="28" height="28" alt=""><span>Pigeonpost</span></div>
"#,
        lang = if v.lang == "tr" { "tr" } else { "en" },
        subject = e(&subject),
    ));
    h.push_str(&format!("<p class=\"lead\">{}</p>\n", e(&lead)));
    t.push_str(&lead);
    t.push_str("\n\n");
    if kind == Kind::Verify {
        let code = v.code.clone().unwrap_or_default();
        h.push_str(&format!(
            "<p class=\"code\">{}</p><p class=\"row\">{}</p>\n",
            e(&code),
            e(w.verify_note)
        ));
        t.push_str(&format!("{code}\n\n{}\n", w.verify_note));
    } else {
        h.push_str(&format!("<h1>{}</h1>\n", e(&v.title)));
        t.push_str(&format!("{}\n\n", v.title));
        h.push_str(&format!(
            "<p class=\"row\"><span class=\"label\">{}</span>{}</p>\n",
            e(w.when),
            e(&v.when)
        ));
        t.push_str(&format!("{}: {}\n", w.when, v.when));
        if !v.location.is_empty() {
            h.push_str(&format!(
                "<p class=\"row\"><span class=\"label\">{}</span>{}</p>\n",
                e(w.where_),
                e(&v.location)
            ));
            t.push_str(&format!("{}: {}\n", w.where_, v.location));
        }
        if let Some(m) = &v.cancel_message {
            h.push_str(&format!(
                "<p class=\"row\"><span class=\"label\">{}</span></p><div class=\"desc\">{}</div>\n",
                e(w.message_from),
                e(m)
            ));
            t.push_str(&format!("\n{}:\n{m}\n", w.message_from));
        }
        if !v.description.is_empty() && kind != Kind::Cancel {
            // Shown as text: links in descriptions are never made clickable here.
            h.push_str(&format!(
                "<div class=\"desc\">{}</div>\n",
                e(&v.description)
            ));
            t.push_str(&format!("\n{}\n", v.description));
        }
        if kind != Kind::Cancel {
            h.push_str("<p style=\"margin:22px 0 8px\">");
            match &v.meeting_url {
                Some(url) => {
                    h.push_str(&format!(
                        "<a class=\"btn\" href=\"{}\">{}</a>",
                        e(url),
                        e(w.join)
                    ));
                    t.push_str(&format!("\n{}: {url}\n", w.join));
                }
                None => {
                    h.push_str(&format!(
                        "<a class=\"btn\" href=\"{}\">{}</a>",
                        e(&v.page_url),
                        e(w.details)
                    ));
                    t.push_str(&format!("\n{}: {}\n", w.details, v.page_url));
                }
            }
            h.push_str("</p>\n");
        }
        if let (Some([yes, maybe, no]), true) =
            (&v.rsvp_urls, matches!(kind, Kind::Invite | Kind::Update))
        {
            h.push_str(&format!(
                "<p class=\"row\" style=\"margin-top:16px\"><span class=\"label\">{}</span></p><p>",
                e(w.going)
            ));
            for (label, url) in [(w.yes, yes), (w.maybe, maybe), (w.no, no)] {
                h.push_str(&format!(
                    "<a class=\"ghost\" href=\"{}\">{}</a>",
                    e(url),
                    e(label)
                ));
                t.push_str(&format!("{label}: {url}\n"));
            }
            h.push_str("</p>\n");
            h.push_str(&format!("<p class=\"foot\">{}</p>\n", e(w.authoritative)));
            t.push_str(&format!("\n{}\n", w.authoritative));
        }
        if matches!(kind, Kind::Invite | Kind::Update | Kind::Cancel) {
            h.push_str(&format!("<p class=\"foot\">{}</p>\n", e(w.attached)));
            t.push_str(&format!("{}\n", w.attached));
        }
    }
    h.push_str("</div><div class=\"foot\">");
    t.push_str("\n--\n");
    if kind != Kind::Verify {
        let why = fill(w.why, v);
        h.push_str(&format!("<p>{}<br>{}</p>", e(&why), e(w.no_reply)));
        t.push_str(&format!("{why}\n{}\n", w.no_reply));
        if let Some(u) = &v.unsubscribe_organizer {
            h.push_str(&format!("<p><a href=\"{}\">{}</a>", e(u), e(w.stop_from)));
            t.push_str(&format!("{}: {u}\n", w.stop_from));
            if let Some(a) = &v.unsubscribe_all {
                h.push_str(&format!(" · <a href=\"{}\">{}</a>", e(a), e(w.stop_all)));
                t.push_str(&format!("{}: {a}\n", w.stop_all));
            }
            h.push_str("</p>");
        }
    } else {
        h.push_str(&format!("<p>{}</p>", e(w.no_reply)));
        t.push_str(&format!("{}\n", w.no_reply));
    }
    h.push_str("</div></div></body></html>");
    (subject, h, t)
}

/// A display name for the From header: "<organizer> via Pigeonpost", cleaned.
pub(crate) fn sender_name(organizer: &str) -> String {
    let clean: String = organizer
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>' | '@' | ','))
        .take(60)
        .collect();
    format!("{} via Pigeonpost", clean.trim())
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
        assert_eq!(
            sender_name("Evil\r\nBcc: x@y <a>"),
            "EvilBcc: xy a via Pigeonpost"
        );
    }
}
