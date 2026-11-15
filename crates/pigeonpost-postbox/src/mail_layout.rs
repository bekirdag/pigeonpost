//! The Pigeonpost email layout shared by every message the postbox renders itself (calendar
//! invitations, updates, cancellations, reminders and guest codes; organization invitations).
//!
//! It is the design kit's email template (`pigeonpost-design-kit/templates/email.html`, published
//! at design.pigeonpost.dev/templates/email.html): a 560 px column on the kit's grey ground, the
//! bird symbol beside a live-text "Pigeonpost" wordmark in Pigeon Navy, a white 16 px-radius
//! card, Signal Blue bulletproof buttons, a sunken detail panel, the six-cell code block and a
//! small footer. The kit tokens are written out as literal colours because mail clients do not
//! read CSS variables; the same palette is mirrored by the Keycloak and saas_be templates.
//!
//! Email-client rules: tables for layout, inline styles for anything that must survive Gmail, a
//! `<style>` block only for dark mode and small screens. No webfonts are fetched (the kit's
//! system stack). The logos are absolute URLs on pigeonpost.dev (`site/email/`, versioned file
//! names) and carry no per-recipient data; there is no tracking pixel. Every message also has a
//! plain-text part, built by the caller.

/// The bird symbol for light mode (87×72, shown at 29×24) and its white variant for dark mode.
pub(crate) const LOGO_LIGHT: &str = "https://pigeonpost.dev/email/pigeonpost-symbol.v2.png";
pub(crate) const LOGO_DARK: &str = "https://pigeonpost.dev/email/pigeonpost-symbol-white.v2.png";

/// The kit's system font stack (no webfont is fetched).
pub(crate) const FONT: &str =
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
pub(crate) const MONO: &str =
    "ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,'Liberation Mono',monospace";

// Kit tokens, light (inline). The `<style>` block remaps the classes to the dark values below.
/// `--pp-bg`: the ground behind the card.
pub(crate) const GROUND: &str = "#F4F6F8";
/// `--pp-surface`: the card.
pub(crate) const CARD: &str = "#FFFFFF";
/// `--pp-border`: card and panel edges, rules.
pub(crate) const RULE: &str = "#E1E5EA";
/// `--pp-border-strong`: secondary button edge.
pub(crate) const RULE_STRONG: &str = "#C9CFD8";
/// The sunken panel and code cells (the kit uses the ground colour).
pub(crate) const SOFT: &str = "#F4F6F8";
/// `--pp-text`: headings and strong text.
pub(crate) const INK: &str = "#12161C";
/// `--pp-text-body`: paragraphs.
pub(crate) const BODY: &str = "#343C47";
/// `--pp-text-muted`: footer and secondary text.
pub(crate) const MUTED: &str = "#59626E";
/// Pigeon Navy (`--pp-brand`): the wordmark.
pub(crate) const BRAND: &str = "#05368B";
/// Signal Blue (`--pp-accent`): primary buttons, in both modes.
pub(crate) const ACCENT: &str = "#086CFF";
/// `--pp-text-link`.
pub(crate) const LINK: &str = "#0A5FE0";

// Kit tokens, dark.
const D_GROUND: &str = "#0B0E12";
const D_CARD: &str = "#141820";
const D_RULE: &str = "#232A35";
const D_RULE_STRONG: &str = "#343D4A";
const D_SOFT: &str = "#10141A";
const D_INK: &str = "#F2F4F7";
const D_BODY: &str = "#D3D8E0";
const D_MUTED: &str = "#A3ACB9";
const D_BRAND: &str = "#8FB4FF";
const D_LINK: &str = "#6EA8FF";

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

/// The card heading (kit: 22/28, bold, `--pp-text`).
pub(crate) fn heading(text: &str) -> String {
    format!(
        "<h1 class=\"pp-ink\" style=\"margin:0 0 16px;font-family:{FONT};font-size:22px;line-height:28px;font-weight:700;color:{INK};\">{}</h1>\n",
        escape(text)
    )
}

/// A body paragraph (kit: 15/24, `--pp-text-body`). `inner_html` is already escaped.
pub(crate) fn para_html(inner_html: &str) -> String {
    format!(
        "<p class=\"pp-body\" style=\"margin:0 0 14px;font-family:{FONT};font-size:15px;line-height:24px;color:{BODY};\">{inner_html}</p>\n"
    )
}

/// Strong inline text inside a paragraph (plain text in, escaped HTML out).
pub(crate) fn strong(text: &str) -> String {
    format!(
        "<strong class=\"pp-ink\" style=\"color:{INK};font-weight:600;\">{}</strong>",
        escape(text)
    )
}

/// The kit's small note under the actions (13/24, body colour).
pub(crate) fn note(text: &str) -> String {
    format!(
        "<p class=\"pp-body\" style=\"margin:0 0 14px;font-family:{FONT};font-size:13px;line-height:24px;color:{BODY};\">{}</p>\n",
        escape(text)
    )
}

/// A closing note set off by a rule (13 px, muted).
pub(crate) fn rule_note(text: &str) -> String {
    format!(
        "<p class=\"pp-muted pp-rule\" style=\"margin:20px 0 0;padding-top:16px;border-top:1px solid {RULE};font-family:{FONT};font-size:13px;line-height:20px;color:{MUTED};\">{}</p>\n",
        escape(text)
    )
}

/// A small uppercase label above a value.
pub(crate) fn label(text: &str) -> String {
    format!(
        "<div class=\"pp-muted\" style=\"font-family:{FONT};font-size:12px;line-height:16px;font-weight:600;text-transform:uppercase;letter-spacing:.04em;color:{MUTED};\">{}</div>",
        escape(text)
    )
}

/// The kit's sunken detail panel (`--pp-surface-sunken`, 12 px radius). `inner_html` is escaped.
pub(crate) fn panel_html(inner_html: &str) -> String {
    format!(
        "<table role=\"presentation\" class=\"pp-panel\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"background-color:{SOFT};border:1px solid {RULE};border-radius:12px;margin:0 0 16px;\"><tr><td style=\"padding:16px 18px;font-family:{FONT};\">{inner_html}</td></tr></table>\n"
    )
}

/// A panel of labelled rows `(label, value)`: each label above its value (14/22, body colour).
pub(crate) fn panel_rows(rows: &[(&str, &str)]) -> String {
    let inner: String = rows
        .iter()
        .enumerate()
        .map(|(i, (l, value))| {
            format!(
                "<div style=\"margin-top:{}px;\">{}<div class=\"pp-body\" style=\"margin-top:2px;font-size:14px;line-height:22px;color:{BODY};\">{}</div></div>",
                if i == 0 { 0 } else { 12 },
                label(l),
                escape(value)
            )
        })
        .collect();
    panel_html(&inner)
}

/// Free text shown verbatim in a panel: escaped, line breaks kept, and never turned into links
/// by us (no `href` is ever built from it).
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
        "{head}<div class=\"pp-body\" style=\"white-space:pre-wrap;font-size:14px;line-height:22px;color:{BODY};\">{}</div>",
        escape(text)
    ))
}

/// The tone of a [`badge`] (the kit's status fills).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Tone::Info => "pp-badge-info",
            Tone::Success => "pp-badge-success",
            Tone::Warning => "pp-badge-warning",
            Tone::Danger => "pp-badge-danger",
        }
    }
    /// (fill, text) in light mode.
    fn light(self) -> (&'static str, &'static str) {
        match self {
            Tone::Info => ("#EDF4FF", "#0B4FB8"),
            Tone::Success => ("#ECFBEE", "#15801D"),
            Tone::Warning => ("#FDF3E1", "#8A5208"),
            Tone::Danger => ("#FDF0EF", "#C42B2B"),
        }
    }
    /// (fill, text) in dark mode.
    fn dark(self) -> (&'static str, &'static str) {
        match self {
            Tone::Info => ("#13233F", "#6EA8FF"),
            Tone::Success => ("#10261A", "#4FDB55"),
            Tone::Warning => ("#2A2010", "#F5B547"),
            Tone::Danger => ("#2B1416", "#FF6B6B"),
        }
    }
}

/// The kit's status pill (12 px bold on a tinted fill), above the heading.
pub(crate) fn badge(text: &str, tone: Tone) -> String {
    let (fill, ink) = tone.light();
    format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 0 14px;\"><tr><td class=\"{cls}\" style=\"padding:5px 10px;border-radius:999px;background-color:{fill};font-family:{FONT};font-size:12px;line-height:14px;font-weight:700;color:{ink};\">{}</td></tr></table>\n",
        escape(text),
        cls = tone.class(),
    )
}

/// One call-to-action. `href` and `label` are plain text.
pub(crate) struct Button<'a> {
    pub href: &'a str,
    pub label: &'a str,
    /// Signal Blue fill; otherwise the outlined secondary style.
    pub primary: bool,
}

/// A row of bulletproof buttons (a table cell with a link inside, 44 px tall). Each button is a
/// left-aligned table, so a row that does not fit wraps on a phone.
pub(crate) fn buttons(list: &[Button<'_>]) -> String {
    let mut out = String::from(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:8px 0 10px;\"><tr><td>",
    );
    for b in list {
        let (cls, fill, edge, ink) = if b.primary {
            ("pp-btn", ACCENT, ACCENT, "#FFFFFF")
        } else {
            ("pp-btn2", CARD, RULE_STRONG, INK)
        };
        out.push_str(&format!(
            "<table role=\"presentation\" align=\"left\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 8px 8px 0;\"><tr><td class=\"{cls}\" bgcolor=\"{fill}\" style=\"border-radius:10px;background-color:{fill};border:1px solid {edge};\"><a class=\"{cls}\" href=\"{href}\" style=\"display:inline-block;padding:12px 22px;font-family:{FONT};font-size:15px;line-height:18px;font-weight:600;color:{ink};text-decoration:none;border-radius:10px;\">{label}</a></td></tr></table>",
            href = escape(b.href),
            label = escape(b.label),
        ));
    }
    out.push_str("</td></tr></table>\n");
    out
}

/// A one-time code. Six digits use the kit's code cells (44×54, a wider gap after the third);
/// anything else is one monospace block. The text part always carries the code as typed.
pub(crate) fn code_block(code: &str) -> String {
    let digits: Vec<char> = code.chars().collect();
    if digits.len() == 6 && digits.iter().all(char::is_ascii_digit) {
        let mut row = String::new();
        for (i, d) in digits.iter().enumerate() {
            row.push_str(&format!(
                "<td class=\"pp-cell\" width=\"44\" height=\"54\" align=\"center\" style=\"width:44px;height:54px;border:1px solid {RULE};border-radius:10px;background-color:{SOFT};font-family:{FONT};font-size:26px;line-height:54px;font-weight:700;color:{INK};text-align:center;\">{d}</td>"
            ));
            if i < 5 {
                let gap = if i == 2 { 14 } else { 6 };
                row.push_str(&format!(
                    "<td width=\"{gap}\" style=\"width:{gap}px;font-size:0;line-height:0;\">&#8202;</td>"
                ));
            }
        }
        format!(
            "<table role=\"presentation\" aria-label=\"{}\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:18px 0 20px;\"><tr>{row}</tr></table>\n",
            escape(code)
        )
    } else {
        format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:18px 0 20px;\"><tr><td class=\"pp-cell\" style=\"border:1px solid {RULE};border-radius:10px;background-color:{SOFT};padding:14px 18px;font-family:{MONO};font-size:26px;line-height:30px;font-weight:700;letter-spacing:4px;color:{INK};\">{}</td></tr></table>\n",
            escape(code)
        )
    }
}

/// A link in the footer (muted, underlined).
pub(crate) fn footer_link(href: &str, text: &str) -> String {
    format!(
        "<a class=\"pp-muted\" href=\"{}\" style=\"color:{MUTED};text-decoration:underline;\">{}</a>",
        escape(href),
        escape(text)
    )
}

/// The last footer line on every message (the Keycloak sign-in code theme uses the same line).
pub(crate) const FOOTER_BRAND: &str =
    "Pigeonpost · Private messaging for people and AI agents · pigeonpost.dev";

/// Wraps rendered card content (already-escaped HTML) in the full document.
///
/// `preheader` is the inbox preview line (plain text). `footer` is already-escaped HTML shown
/// under the card in the muted style; [`FOOTER_BRAND`] is appended as its last line.
pub(crate) fn document(
    lang: &str,
    title: &str,
    preheader: &str,
    card: &str,
    footer: &str,
) -> String {
    let lang = if lang == "tr" { "tr" } else { "en" };
    let pre = if preheader.is_empty() {
        String::new()
    } else {
        format!(
            "<div style=\"display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden;mso-hide:all;color:{GROUND};\">{}&#8199;&#65279;&#847;&#8199;&#65279;&#847;&#8199;&#65279;&#847;</div>\n",
            escape(preheader)
        )
    };
    let footer = if footer.is_empty() {
        escape(FOOTER_BRAND)
    } else {
        format!("{footer}<br>{}", escape(FOOTER_BRAND))
    };
    let mut badges = String::new();
    for t in [Tone::Info, Tone::Success, Tone::Warning, Tone::Danger] {
        let (fill, ink) = t.dark();
        badges.push_str(&format!(
            "    .{} {{ background-color: {fill} !important; color: {ink} !important; }}\n",
            t.class()
        ));
    }
    format!(
        r#"<!DOCTYPE html>
<html lang="{lang}" xmlns="http://www.w3.org/1999/xhtml">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="x-apple-disable-message-reformatting">
<meta name="format-detection" content="telephone=no, date=no, address=no, email=no, url=no">
<meta name="color-scheme" content="light dark">
<meta name="supported-color-schemes" content="light dark">
<title>{title}</title>
<style>
  :root {{ color-scheme: light dark; supported-color-schemes: light dark; }}
  body {{ margin: 0 !important; padding: 0 !important; width: 100% !important; }}
  a {{ color: {LINK}; }}
  @media (prefers-color-scheme: dark) {{
    .pp-bg {{ background-color: {D_GROUND} !important; }}
    .pp-card {{ background-color: {D_CARD} !important; border-color: {D_RULE} !important; }}
    .pp-brand {{ color: {D_BRAND} !important; }}
    .pp-ink {{ color: {D_INK} !important; }}
    .pp-body {{ color: {D_BODY} !important; }}
    .pp-muted, .pp-muted a, a.pp-muted {{ color: {D_MUTED} !important; }}
    .pp-rule {{ border-color: {D_RULE} !important; }}
    .pp-panel, .pp-cell {{ background-color: {D_SOFT} !important; border-color: {D_RULE} !important; }}
    .pp-cell {{ color: {D_INK} !important; }}
    td.pp-btn2 {{ background-color: {D_CARD} !important; border-color: {D_RULE_STRONG} !important; }}
    a.pp-btn2 {{ color: {D_INK} !important; }}
    a {{ color: {D_LINK}; }}
{badges}    .pp-logo-light {{ display: none !important; }}
    .pp-logo-dark {{ display: block !important; max-height: none !important; overflow: visible !important; }}
  }}
  /* Outlook.com dark mode */
  [data-ogsc] .pp-brand {{ color: {D_BRAND} !important; }}
  [data-ogsc] .pp-ink {{ color: {D_INK} !important; }}
  [data-ogsc] .pp-body {{ color: {D_BODY} !important; }}
  [data-ogsc] .pp-muted {{ color: {D_MUTED} !important; }}
  [data-ogsc] .pp-logo-light {{ display: none !important; }}
  [data-ogsc] .pp-logo-dark {{ display: block !important; max-height: none !important; overflow: visible !important; }}
  @media only screen and (max-width: 520px) {{
    .pp-outer {{ padding: 20px 16px !important; }}
    .pp-card-pad {{ padding: 26px 20px 22px !important; }}
  }}
</style>
</head>
<body class="pp-bg" style="margin:0;padding:0;background-color:{GROUND};">
{pre}<table role="presentation" class="pp-bg" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:{GROUND};">
  <tr>
    <td class="pp-outer" align="center" style="padding:32px 16px;">
      <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="max-width:560px;">
        <tr>
          <td style="padding:0 4px 18px;">
            <table role="presentation" cellpadding="0" cellspacing="0" border="0">
              <tr>
                <td width="29" style="width:29px;">
                  <img class="pp-logo-light" src="{LOGO_LIGHT}" width="29" height="24" alt="" style="display:block;border:0;outline:none;width:29px;height:24px;">
                  <!--[if !mso]><!-->
                  <div class="pp-logo-dark" style="display:none;max-height:0;overflow:hidden;mso-hide:all;">
                    <img src="{LOGO_DARK}" width="29" height="24" alt="" style="display:block;border:0;outline:none;width:29px;height:24px;">
                  </div>
                  <!--<![endif]-->
                </td>
                <td class="pp-brand" style="padding-left:8px;font-family:{FONT};font-size:17px;line-height:24px;font-weight:700;color:{BRAND};">Pigeonpost</td>
              </tr>
            </table>
          </td>
        </tr>
        <tr>
          <td class="pp-card pp-card-pad" style="background-color:{CARD};border:1px solid {RULE};border-radius:16px;padding:32px 32px 28px;font-family:{FONT};font-size:15px;line-height:24px;color:{BODY};">
{card}
          </td>
        </tr>
        <tr>
          <td class="pp-muted" style="padding:18px 8px 0;font-family:{FONT};font-size:12px;line-height:18px;color:{MUTED};">
{footer}
          </td>
        </tr>
      </table>
    </td>
  </tr>
</table>
</body>
</html>"#,
        title = escape(title),
    )
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
    fn layout_uses_the_versioned_logos_and_no_tracking() {
        let html = document("en", "T <x>", "pre", "<p>card</p>", "foot");
        assert!(html.contains(LOGO_LIGHT) && html.contains(LOGO_DARK));
        assert!(html.contains("width=\"29\" height=\"24\""));
        assert!(!html.contains("pigeonpost.dev/logo.png"));
        assert!(html.contains("<title>T &lt;x&gt;</title>"));
        assert!(html.contains("prefers-color-scheme: dark"));
        // Only the two logos are images: no pixel.
        assert_eq!(html.matches("<img").count(), 2);
        // Every image is an absolute, versioned pigeonpost.dev URL.
        for src in html.split("src=\"").skip(1) {
            let url = &src[..src.find('"').unwrap()];
            assert!(url.starts_with("https://pigeonpost.dev/email/"), "{url}");
            assert!(url.contains(".v") && url.ends_with(".png"), "{url}");
        }
        // No webfont, no external stylesheet, no script.
        for banned in [
            "@import",
            "@font-face",
            "<link",
            "<script",
            "fonts.googleapis",
        ] {
            assert!(!html.contains(banned), "{banned}");
        }
        // The brand line closes the footer.
        assert!(html.contains(&format!("foot<br>{}", escape(FOOTER_BRAND))));
    }

    #[test]
    fn layout_uses_the_kit_palette_and_frame() {
        let html = document("tr", "T", "", "", "");
        assert!(html.starts_with("<!DOCTYPE html>\n<html lang=\"tr\""));
        // Kit light tokens inline: ground, card edge, 16 px card, 560 px column, navy wordmark.
        for token in [
            "background-color:#F4F6F8",
            "border:1px solid #E1E5EA;border-radius:16px",
            "max-width:560px",
            "color:#05368B;\">Pigeonpost</td>",
            "<meta name=\"color-scheme\" content=\"light dark\">",
        ] {
            assert!(html.contains(token), "{token}");
        }
        // Kit dark tokens in the media query.
        for token in [
            "#0B0E12", "#141820", "#232A35", "#F2F4F7", "#D3D8E0", "#8FB4FF",
        ] {
            assert!(html.contains(token), "{token}");
        }
        // The pre-kit palette is gone.
        for old in ["#2563eb", "#3b82f6", "#14181f", "Geist"] {
            assert!(!html.contains(old), "{old}");
        }
        // No preheader when none is given.
        assert!(!html.contains("display:none;font-size:1px"));
    }

    #[test]
    fn buttons_are_bulletproof_signal_blue_cells_with_escaped_links() {
        let html = buttons(&[
            Button {
                href: "https://x.test/a?b=1&c=\"2\"",
                label: "Go <now>",
                primary: true,
            },
            Button {
                href: "https://x.test/2",
                label: "Later",
                primary: false,
            },
        ]);
        assert!(html.contains("bgcolor=\"#086CFF\""));
        assert!(html.contains("href=\"https://x.test/a?b=1&amp;c=&quot;2&quot;\""));
        assert!(html.contains(">Go &lt;now&gt;</a>"));
        assert!(html.contains("class=\"pp-btn2\" bgcolor=\"#FFFFFF\""));
        // 12 + 12 padding + 18 line + 2 border = 44 px tall.
        assert!(html.contains("padding:12px 22px;") && html.contains("line-height:18px;"));
        assert_eq!(html.matches("align=\"left\"").count(), 2);
    }

    #[test]
    fn six_digit_codes_use_the_kit_cells_and_others_one_block() {
        let html = code_block("482915");
        assert_eq!(html.matches("class=\"pp-cell\"").count(), 6);
        assert!(html.contains("aria-label=\"482915\""));
        assert!(html.contains(">4</td>") && html.contains(">5</td>"));
        assert!(html.contains("width:14px;"));
        let other = code_block("AB<12>");
        assert_eq!(other.matches("class=\"pp-cell\"").count(), 1);
        assert!(other.contains("AB&lt;12&gt;"));
    }

    #[test]
    fn text_panels_never_link_and_badges_carry_dark_classes() {
        let html = text_panel(Some("Note"), "See https://evil.example/x <b>\nline 2");
        assert!(!html.contains("href"));
        assert!(html.contains("https://evil.example/x &lt;b&gt;\nline 2"));
        assert!(html.contains("white-space:pre-wrap"));
        let b = badge("Cancelled", Tone::Danger);
        assert!(b.contains("pp-badge-danger") && b.contains("#FDF0EF") && b.contains("#C42B2B"));
        let doc = document("en", "T", "", &b, "");
        assert!(doc.contains(
            ".pp-badge-danger { background-color: #2B1416 !important; color: #FF6B6B !important; }"
        ));
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
