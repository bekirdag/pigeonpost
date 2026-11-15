//! The Pigeonpost email layout shared by every message the postbox renders itself (calendar
//! invitations, updates, cancellations, reminders and guest codes; organization invitations).
//!
//! The outer frame is `mail_frame.html`: "Pigeonpost email frame v1", the one skeleton every
//! Pigeonpost email uses. It is cut from the Keycloak theme (sealunit/su_iam
//! `keycloak/themes/pigeonpost/email/html/template.ftl`), which renders exactly this, and follows
//! the design kit's template (design.pigeonpost.dev/templates/email.html): a 560 px column on the
//! kit ground, the bird symbol beside a live-text wordmark in Pigeon Navy, a white 16 px card, the
//! kit dark palette, and the footer line "Pigeonpost · Piyote Bilişim Ltd. Şti. · pigeonpost.dev"
//! (the company's legal name). Sign-in codes, saas_be receipts, calendar and organization messages
//! therefore share one frame byte for byte. The frame carries the `pp-email-frame v1` marker,
//! so saas_be passes it through unchanged instead of wrapping it a second time. The store adapter
//! keeps a copy (its tests compare it).
//!
//! The card blocks below use the frame's component styles and its class names only (`pp-h`,
//! `pp-p`, `pp-foot`, `pp-box`, `pp-cell`, `pp-rule`, `pp-a`), so the frame's one `<style>` block
//! covers their dark mode. Email-client rules: tables for layout, inline styles, no webfont, no
//! tracking pixel; the logos are absolute, versioned URLs on pigeonpost.dev. Every message also
//! has a plain-text part, built by the caller.

/// The shared frame, with `{{PREHEADER}}`, `{{CONTENT}}` and `{{REASON}}` placeholders.
pub(crate) const FRAME: &str = include_str!("mail_frame.html");

/// The bird symbol for light mode and its white variant for dark mode, as the frame names them
/// (tests check the frame against these).
#[cfg(test)]
pub(crate) const LOGO_LIGHT: &str = "https://pigeonpost.dev/email/pigeonpost-symbol.v1.png";
#[cfg(test)]
pub(crate) const LOGO_DARK: &str = "https://pigeonpost.dev/email/pigeonpost-symbol-white.v1.png";

/// The frame's last footer line.
#[cfg(test)]
pub(crate) const FOOTER_BRAND: &str = "Pigeonpost · Piyote Bilişim Ltd. Şti. · pigeonpost.dev";

/// The kit's system font stack (no webfont is fetched).
pub(crate) const FONT: &str =
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
const MONO: &str =
    "ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,'Liberation Mono',monospace";

// Kit tokens, light (inline); the frame's `<style>` block remaps the classes for dark mode.
const RULE: &str = "#E1E5EA";
const RULE_STRONG: &str = "#C9CFD8";
const SOFT: &str = "#F4F6F8";
const INK: &str = "#12161C";
const BODY: &str = "#343C47";
const MUTED: &str = "#59626E";
const ACCENT: &str = "#086CFF";

pub(crate) fn escape(s: &str) -> String {
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

// ---- card content blocks (inputs are plain text unless a name says `_html`) --------------------

/// The frame's `h1` component.
pub(crate) fn heading(text: &str) -> String {
    format!(
        "<h1 class=\"pp-h\" style=\"margin:0 0 16px;font-family:{FONT};font-size:22px;line-height:28px;font-weight:700;color:{INK};\">{}</h1>\n",
        escape(text)
    )
}

/// The frame's `p` component. `inner_html` is already escaped.
pub(crate) fn para_html(inner_html: &str) -> String {
    format!(
        "<p class=\"pp-p\" style=\"margin:0 0 14px;font-family:{FONT};font-size:15px;line-height:24px;color:{BODY};\">{inner_html}</p>\n"
    )
}

/// Strong inline text inside a paragraph (plain text in, escaped HTML out).
pub(crate) fn strong(text: &str) -> String {
    format!("<strong>{}</strong>", escape(text))
}

/// The frame's `small` component (13/24, body colour).
pub(crate) fn note(text: &str) -> String {
    format!(
        "<p class=\"pp-p\" style=\"margin:0 0 14px;font-family:{FONT};font-size:13px;line-height:24px;color:{BODY};\">{}</p>\n",
        escape(text)
    )
}

/// A closing note set off by a rule (13 px, muted).
pub(crate) fn rule_note(text: &str) -> String {
    format!(
        "<p class=\"pp-foot pp-rule\" style=\"margin:20px 0 0;padding-top:16px;border-top:1px solid {RULE};font-family:{FONT};font-size:13px;line-height:20px;color:{MUTED};\">{}</p>\n",
        escape(text)
    )
}

/// A small uppercase label above a value (muted).
pub(crate) fn label(text: &str) -> String {
    format!(
        "<div class=\"pp-foot\" style=\"font-family:{FONT};font-size:12px;line-height:16px;font-weight:600;text-transform:uppercase;letter-spacing:.04em;color:{MUTED};\">{}</div>",
        escape(text)
    )
}

/// The frame's `box` component (sunken panel). `inner_html` is escaped.
pub(crate) fn panel_html(inner_html: &str) -> String {
    format!(
        "<table role=\"presentation\" class=\"pp-box\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"background:{SOFT};border:1px solid {RULE};border-radius:12px;margin:0 0 14px;\"><tr><td style=\"padding:16px 18px;font-family:{FONT};\">{inner_html}</td></tr></table>\n"
    )
}

/// A box of labelled rows `(label, value)`: each label above its value (14/22, body colour).
pub(crate) fn panel_rows(rows: &[(&str, &str)]) -> String {
    let inner: String = rows
        .iter()
        .enumerate()
        .map(|(i, (l, value))| {
            format!(
                "<div style=\"margin-top:{}px;\">{}<div class=\"pp-p\" style=\"margin-top:2px;font-size:14px;line-height:22px;color:{BODY};\">{}</div></div>",
                if i == 0 { 0 } else { 12 },
                label(l),
                escape(value)
            )
        })
        .collect();
    panel_html(&inner)
}

/// Free text shown verbatim in a box: escaped, line breaks kept, and never turned into links by
/// us (no `href` is ever built from it).
pub(crate) fn text_panel(label_text: Option<&str>, text: &str) -> String {
    let head = label_text
        .map(|l| {
            format!(
                "{}<div style=\"height:4px;line-height:4px;font-size:4px;\">&#8202;</div>",
                label(l)
            )
        })
        .unwrap_or_default();
    panel_html(&format!(
        "{head}<div class=\"pp-p\" style=\"white-space:pre-wrap;font-size:14px;line-height:22px;color:{BODY};\">{}</div>",
        escape(text)
    ))
}

/// The tone of a [`badge`] (the kit's status fills in light mode; a neutral box in dark mode,
/// since the shared frame's style block has no per-tone rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Info,
    Danger,
    Warning,
}

impl Tone {
    /// (fill, text) in light mode.
    fn light(self) -> (&'static str, &'static str) {
        match self {
            Tone::Info => ("#EDF4FF", "#0B4FB8"),
            Tone::Warning => ("#FDF3E1", "#8A5208"),
            Tone::Danger => ("#FDF0EF", "#C42B2B"),
        }
    }
}

/// The kit's status pill (12 px bold on a tinted fill), above the heading.
pub(crate) fn badge(text: &str, tone: Tone) -> String {
    let (fill, ink) = tone.light();
    format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 0 14px;\"><tr><td class=\"pp-box\" style=\"padding:5px 10px;border-radius:999px;background:{fill};border:1px solid {fill};font-family:{FONT};font-size:12px;line-height:14px;font-weight:700;\"><span class=\"pp-h\" style=\"color:{ink};\">{}</span></td></tr></table>\n",
        escape(text)
    )
}

/// One call-to-action. `href` and `label` are plain text.
pub(crate) struct Button<'a> {
    pub href: &'a str,
    pub label: &'a str,
    /// Signal Blue fill (the frame's `button` component); otherwise an outlined secondary.
    pub primary: bool,
}

/// A row of bulletproof buttons in the frame's `button` style. Each is a left-aligned table, so a
/// row that does not fit wraps on a phone.
pub(crate) fn buttons(list: &[Button<'_>]) -> String {
    let mut out = String::from(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:24px 0 8px;\"><tr><td>",
    );
    for b in list {
        let href = escape(b.href);
        let label = escape(b.label);
        if b.primary {
            out.push_str(&format!(
                "<table role=\"presentation\" align=\"left\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 8px 8px 0;\"><tr><td style=\"border-radius:10px;background:{ACCENT};\"><a href=\"{href}\" style=\"display:inline-block;padding:13px 22px;font-family:{FONT};font-size:15px;font-weight:600;color:#FFFFFF;text-decoration:none;border-radius:10px;\">{label}</a></td></tr></table>"
            ));
        } else {
            out.push_str(&format!(
                "<table role=\"presentation\" align=\"left\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 8px 8px 0;\"><tr><td class=\"pp-box\" style=\"border-radius:10px;background:#FFFFFF;border:1px solid {RULE_STRONG};\"><a class=\"pp-h\" href=\"{href}\" style=\"display:inline-block;padding:12px 21px;font-family:{FONT};font-size:15px;font-weight:600;color:{INK};text-decoration:none;border-radius:10px;\">{label}</a></td></tr></table>"
            ));
        }
    }
    out.push_str("</td></tr></table>\n");
    out
}

/// A one-time code. Six digits use the frame's code cells (6 px spacers, 14 px after the third);
/// anything else is one monospace cell. The text part always carries the code as typed.
pub(crate) fn code_block(code: &str) -> String {
    let digits: Vec<char> = code.chars().collect();
    let cell = |inner: &str, extra: &str| {
        format!(
            "<td class=\"pp-cell\" style=\"{extra}border:1px solid {RULE};border-radius:10px;background:{SOFT};font-size:26px;font-weight:700;color:{INK};text-align:center;\">{inner}</td>"
        )
    };
    if digits.len() == 6 && digits.iter().all(char::is_ascii_digit) {
        let mut row = String::new();
        for (i, d) in digits.iter().enumerate() {
            row.push_str(&cell(
                &d.to_string(),
                &format!("width:44px;height:54px;font-family:{FONT};"),
            ));
            if i < 5 {
                row.push_str(if i == 2 {
                    "<td style=\"width:14px;\"></td>"
                } else {
                    "<td style=\"width:6px;\"></td>"
                });
            }
        }
        format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:18px 0 20px;\"><tr>{row}</tr></table>\n"
        )
    } else {
        format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:18px 0 20px;\"><tr>{}</tr></table>\n",
            cell(
                &escape(code),
                &format!("padding:14px 18px;letter-spacing:4px;font-family:{MONO};")
            )
        )
    }
}

/// A link in the footer (muted, underlined).
pub(crate) fn footer_link(href: &str, text: &str) -> String {
    format!(
        "<a class=\"pp-foot\" href=\"{}\" style=\"color:{MUTED};text-decoration:underline;\">{}</a>",
        escape(href),
        escape(text)
    )
}

/// The full message: [`FRAME`] with the preheader (plain text), the card (escaped HTML) and the
/// footer reason (escaped HTML; the frame adds [`FOOTER_BRAND`] under it). Only the `lang`
/// attribute differs from the shared skeleton.
pub(crate) fn document(lang: &str, preheader: &str, card: &str, reason: &str) -> String {
    let lang = if lang == "tr" { "tr" } else { "en" };
    // Split the frame once at its placeholders, so text that happens to contain a placeholder
    // can never be substituted again.
    let (head, rest) = FRAME.split_once("{{PREHEADER}}").expect("frame preheader");
    let (mid, rest) = rest.split_once("{{CONTENT}}").expect("frame content");
    let (tail_a, tail_b) = rest.split_once("{{REASON}}").expect("frame reason");
    let head = head.replacen("<html lang=\"en\"", &format!("<html lang=\"{lang}\""), 1);
    let mut out = String::with_capacity(FRAME.len() + card.len() + 512);
    out.push_str(&head);
    out.push_str(&escape(preheader));
    out.push_str(mid);
    out.push_str(card.trim_end_matches('\n'));
    out.push_str(tail_a);
    out.push_str(reason);
    out.push_str(tail_b);
    out
}

// ---- who a message is from ----------------------------------------------------------------------

/// Short handle segments that read as acronyms (the Phase 1d list in `site-inbox/names.js` and the
/// native clients; edit them together).
const ACRONYMS: [&str; 12] = [
    "hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev",
];

/// The Phase 1d name for a handle: `/bekir/main` -> "Bekir", `/wodo/hr` -> "Wodo HR",
/// `/acme/john-doe` -> "Acme John Doe". `None` for key addresses (`/k/…`) and for anything that is
/// not a name-shaped handle (address-style handles, non-slash strings), so a raw key or address
/// never reaches a message header or greeting.
pub(crate) fn handle_name(handle: &str) -> Option<String> {
    let handle = handle.trim();
    if !handle.starts_with('/') || handle.starts_with("/k/") || handle.contains('@') {
        return None;
    }
    let mut parts: Vec<&str> = handle.split('/').filter(|p| !p.is_empty()).collect();
    if parts.first() == Some(&"github") {
        return parts.get(1).map(|s| s.to_string());
    }
    if parts.len() > 1 && parts.last() == Some(&"main") {
        parts.pop();
    }
    let word = |w: &str| {
        let lower = w.to_lowercase();
        if ACRONYMS.contains(&lower.as_str()) {
            lower.to_uppercase()
        } else {
            let mut c = lower.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    };
    let name = parts
        .iter()
        .flat_map(|p| p.split(['-', '_']))
        .filter(|w| !w.is_empty())
        .map(word)
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty()).then_some(name)
}

/// Longest name shown before " via Pigeonpost", in characters.
pub(crate) const SENDER_NAME_MAX: usize = 40;

/// Invisible, reordering and structure-faking characters (as `profile_names.rs` refuses them).
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
            | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{2800}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF0}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}')
}

/// A person's name made safe for a `From` display name and a greeting: no CR/LF or other
/// controls, no invisible characters, none of `" \ < > @ , ; : ( ) [ ]` (which could close the
/// display name, add an address or a second mailbox, or pose as one), whitespace collapsed,
/// at most [`SENDER_NAME_MAX`] characters. `None` when nothing printable is left.
pub(crate) fn header_safe_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else if invisible(c)
                || matches!(
                    c,
                    '"' | '\\' | '<' | '>' | '@' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
                )
            {
                '\u{0}'
            } else {
                c
            }
        })
        .filter(|c| *c != '\u{0}')
        .collect();
    let name: String = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SENDER_NAME_MAX)
        .collect::<String>()
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// The `From` display name: "<name> via Pigeonpost" for a known name, else `fallback`
/// (e.g. "Invitation via Pigeonpost"). The name is cleaned with [`header_safe_name`] and sent
/// as plain UTF-8: saas_be builds the header (`pigeonpostFromHeader`), quoting ASCII names and
/// RFC 2047-encoding the others as unquoted encoded-words.
pub(crate) fn sender_display(name: Option<&str>, fallback: &str) -> String {
    match name.and_then(header_safe_name) {
        Some(n) => format!("{n} via Pigeonpost"),
        None => fallback.to_string(),
    }
}

/// A mirror of saas_be's `PIGEONPOST_PAYLOAD_SCHEMAS` (sealunit/saas_be
/// `src/modules/notifications/pigeonpost-novu-workflows.ts`): the Novu trigger payload is the
/// request's `payload` merged with `tenant_id`, `subject`, `html` and `text`
/// (`notification-manager.service.ts`), then validated against the workflow's zod schema. A
/// payload that fails it is never delivered. zod's `.optional()` accepts a missing key, not null.
#[cfg(test)]
pub(crate) mod contract {
    use serde_json::{json, Value};

    /// What saas_be hands Novu for one internal email request.
    pub(crate) fn merged(
        payload: &Value,
        tenant: &str,
        subject: &str,
        html: &str,
        text: &str,
    ) -> Value {
        let mut m = payload.as_object().cloned().unwrap_or_default();
        m.insert("tenant_id".into(), json!(tenant));
        m.insert("subject".into(), json!(subject));
        m.insert("html".into(), json!(html));
        m.insert("text".into(), json!(text));
        Value::Object(m)
    }

    fn string(p: &Value, key: &str, min: usize, max: usize, optional: bool) -> Result<(), String> {
        match p.get(key) {
            None if optional => Ok(()),
            Some(Value::String(s)) if (min..=max).contains(&s.encode_utf16().count()) => Ok(()),
            other => Err(format!("{key}: invalid ({other:?})")),
        }
    }

    fn single_line(p: &Value, key: &str, max: usize) -> Result<(), String> {
        string(p, key, 1, max, false)?;
        if p[key]
            .as_str()
            .unwrap_or_default()
            .contains(['\r', '\n', '\0'])
        {
            return Err(format!("{key}: must be a single line"));
        }
        Ok(())
    }

    fn ident(p: &Value, key: &str) -> Result<(), String> {
        string(p, key, 1, 200, false)
    }

    fn uint(p: &Value, key: &str) -> Result<(), String> {
        match p.get(key) {
            Some(Value::Number(n)) if n.as_u64().is_some() => Ok(()),
            other => Err(format!("{key}: not a non-negative integer ({other:?})")),
        }
    }

    /// `Ok(())` when saas_be would accept `p` (the merged payload) for `workflow`.
    pub(crate) fn check(workflow: &str, p: &Value) -> Result<(), String> {
        if p["tenant_id"] != "pigeonpost" {
            return Err("tenant_id: must be pigeonpost".into());
        }
        single_line(p, "subject", 998)?;
        string(p, "html", 1, 512_000, false)?;
        string(p, "text", 1, 256_000, false)?;
        ident(p, "delivery_id")?;
        string(p, "locale", 2, 35, true)?;
        let meeting = |method: Option<&str>| -> Result<(), String> {
            ident(p, "event_id")?;
            if p.get("occurrence_id").is_some() {
                ident(p, "occurrence_id")?;
            }
            uint(p, "sequence")?;
            single_line(p, "organizer_handle", 320)?;
            if let Some(m) = method {
                if p["ics_method"] != m {
                    return Err(format!("ics_method: expected {m}"));
                }
            }
            Ok(())
        };
        match workflow {
            "pigeonpost-meeting-invite" | "pigeonpost-meeting-update" => meeting(Some("REQUEST")),
            "pigeonpost-meeting-cancel" => meeting(Some("CANCEL")),
            "pigeonpost-meeting-reminder" => {
                meeting(None)?;
                ident(p, "reminder_id")
            }
            "pigeonpost-meeting-verify" => {
                ident(p, "event_id")?;
                ident(p, "challenge_id")
            }
            "pigeonpost-org-invite" => {
                ident(p, "org_id")?;
                ident(p, "invite_id")
            }
            other => Err(format!("unknown workflow {other}")),
        }
    }

    #[test]
    fn the_mirror_rejects_what_saas_be_rejects() {
        let base = |extra: Value| {
            let mut v = merged(&extra, "pigeonpost", "Subject", "<p>h</p>", "t");
            v["delivery_id"] = json!("dlv_1");
            v
        };
        // The pre-fix guest verification payload: no event_id.
        let old = base(json!({ "challenge_id": "c1" }));
        assert!(check("pigeonpost-meeting-verify", &old)
            .unwrap_err()
            .starts_with("event_id"));
        let ok = base(json!({ "event_id": "e1", "challenge_id": "c1" }));
        assert_eq!(check("pigeonpost-meeting-verify", &ok), Ok(()));
        // A null challenge is not "optional": it fails.
        let null = base(json!({ "event_id": "e1", "challenge_id": null }));
        assert!(check("pigeonpost-meeting-verify", &null).is_err());
        let mut multi = ok.clone();
        multi["subject"] = json!("a\nb");
        assert!(check("pigeonpost-meeting-verify", &multi).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_is_the_shared_skeleton_with_its_marker_and_footer() {
        // Placeholders appear once each; the marker lets saas_be skip a second wrap.
        for p in ["{{PREHEADER}}", "{{CONTENT}}", "{{REASON}}"] {
            assert_eq!(FRAME.matches(p).count(), 1, "{p}");
        }
        assert!(FRAME.contains("<body class=\"pp-bg\" style=\"margin:0;padding:0;background:#F4F6F8;\">\n<!-- pp-email-frame v1 -->"));
        assert!(FRAME.contains(&format!("{{{{REASON}}}}<br>{FOOTER_BRAND}</td>")));
        assert!(FRAME.contains(LOGO_LIGHT) && FRAME.contains(LOGO_DARK));
        // Kit tokens: ground, card, navy wordmark, 560 px column, dark palette.
        for token in [
            "background:#F4F6F8",
            "border:1px solid #E1E5EA;border-radius:16px",
            "max-width:560px",
            "color:#05368B;\">Pigeonpost</td>",
            "@media (prefers-color-scheme: dark)",
            "#0B0E12",
            "#141820",
            "#8FB4FF",
        ] {
            assert!(FRAME.contains(token), "{token}");
        }
        // The kit's placeholder street address is never shipped.
        assert!(!FRAME.contains("Caferağa"));
    }

    #[test]
    fn document_fills_the_frame_and_changes_nothing_else() {
        let html = document("tr", "pre <x>", "<p>card</p>\n", "why");
        let expected = FRAME
            .replacen("<html lang=\"en\"", "<html lang=\"tr\"", 1)
            .replace("{{PREHEADER}}", "pre &lt;x&gt;")
            .replace("{{CONTENT}}", "<p>card</p>")
            .replace("{{REASON}}", "why");
        assert_eq!(html, expected);
        // Only the two logos are images: no pixel, no script, no webfont.
        assert_eq!(html.matches("<img").count(), 2);
        for banned in ["@import", "@font-face", "<link", "<script"] {
            assert!(!html.contains(banned), "{banned}");
        }
        // A placeholder inside user text is never substituted again.
        let sneaky = document("en", "{{CONTENT}}", "c", "{{PREHEADER}}");
        assert_eq!(sneaky.matches("{{CONTENT}}").count(), 1);
        assert_eq!(sneaky.matches("{{PREHEADER}}").count(), 1);
    }

    #[test]
    fn blocks_use_only_classes_the_frame_styles() {
        let frame_classes = [
            "pp-bg",
            "pp-card",
            "pp-h",
            "pp-p",
            "pp-foot",
            "pp-word",
            "pp-box",
            "pp-cell",
            "pp-rule",
            "pp-ok",
            "pp-a",
            "pp-light-only",
            "pp-dark-only",
            "pp-pad",
        ];
        let all = [
            heading("h"),
            para_html("p"),
            note("n"),
            rule_note("r"),
            panel_rows(&[("a", "b"), ("c", "d")]),
            text_panel(Some("l"), "t"),
            badge("b", Tone::Danger),
            buttons(&[
                Button {
                    href: "h",
                    label: "l",
                    primary: true,
                },
                Button {
                    href: "h",
                    label: "l",
                    primary: false,
                },
            ]),
            code_block("123456"),
            code_block("AB"),
            footer_link("h", "t"),
        ]
        .concat();
        for chunk in all.split("class=\"").skip(1) {
            for class in chunk[..chunk.find('"').unwrap()].split(' ') {
                assert!(frame_classes.contains(&class), "{class}");
                assert!(FRAME.contains(&format!(".{class}")), "{class} not styled");
            }
        }
    }

    #[test]
    fn buttons_match_the_frame_component_and_escape() {
        let html = buttons(&[Button {
            href: "https://x.test/a?b=1&c=\"2\"",
            label: "Go <now>",
            primary: true,
        }]);
        assert!(html.contains("<td style=\"border-radius:10px;background:#086CFF;\"><a href=\"https://x.test/a?b=1&amp;c=&quot;2&quot;\" style=\"display:inline-block;padding:13px 22px;"));
        assert!(html.contains(">Go &lt;now&gt;</a>"));
    }

    #[test]
    fn code_cells_match_the_frame_and_other_codes_are_one_block() {
        let html = code_block("470736");
        assert_eq!(html.matches("class=\"pp-cell\"").count(), 6);
        assert_eq!(html.matches("<td style=\"width:14px;\"></td>").count(), 1);
        assert_eq!(html.matches("<td style=\"width:6px;\"></td>").count(), 4);
        let other = code_block("AB<12>");
        assert_eq!(other.matches("class=\"pp-cell\"").count(), 1);
        assert!(other.contains("AB&lt;12&gt;"));
    }

    #[test]
    fn text_panels_never_link() {
        let html = text_panel(Some("Note"), "See https://evil.example/x <b>\nline 2");
        assert!(!html.contains("href"));
        assert!(html.contains("https://evil.example/x &lt;b&gt;\nline 2"));
        assert!(html.contains("white-space:pre-wrap"));
    }
}
#[cfg(test)]
mod sender_tests {
    use super::*;

    #[test]
    fn test_handle_name() {
        assert_eq!(handle_name("/bekir/main"), Some("Bekir".to_string()));
        assert_eq!(handle_name("/wodo/hr"), Some("Wodo HR".to_string()));
        assert_eq!(
            handle_name("/acme/john-doe"),
            Some("Acme John Doe".to_string())
        );
        assert_eq!(handle_name("/k/h30jr50e4km9x5j7ad1xtv28mr"), None);
        assert_eq!(handle_name("/alex@example.com"), None);
        assert_eq!(handle_name("bekir"), None);
    }

    #[test]
    fn test_header_safe_name() {
        let result = header_safe_name("Evil\r\nBcc: x@y.com <a@b>");
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(!s.contains('\r'));
        assert!(!s.contains('\n'));
        assert!(!s.contains('<'));
        assert!(!s.contains('>'));
        assert!(!s.contains('@'));
        assert!(!s.contains(':'));
        assert!(!s.contains('"'));

        assert_eq!(header_safe_name("\u{202E}\u{200B}  "), None);

        let long_name = "a".repeat(100);
        let truncated = header_safe_name(&long_name).unwrap();
        assert!(truncated.len() <= SENDER_NAME_MAX);
    }

    #[test]
    fn test_sender_display() {
        assert_eq!(
            sender_display(Some("Bekir"), "Invitation via Pigeonpost"),
            "Bekir via Pigeonpost"
        );
        assert_eq!(
            sender_display(None, "Invitation via Pigeonpost"),
            "Invitation via Pigeonpost"
        );
        assert_eq!(
            sender_display(Some("\r\n<>"), "Invitation via Pigeonpost"),
            "Invitation via Pigeonpost"
        );

        // Non-ASCII stays plain UTF-8 here; saas_be encodes it for the header.
        assert_eq!(sender_display(Some("Çağrı"), "X"), "Çağrı via Pigeonpost");
    }
}
