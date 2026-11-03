//! The Pigeonpost email layout shared by every message the postbox renders itself (calendar
//! invitations, updates, cancellations, reminders and guest codes; organization invitations).
//!
//! It mirrors the branded Keycloak email theme (sealunit/su_iam
//! `keycloak/themes/pigeonpost/email/html/template.ftl`) so a sign-in code and a meeting
//! invitation look like the same product: the light/dark wordmark above a white card, the same
//! palette, type stack and footer.
//!
//! Email-client rules: tables for layout, inline styles for anything that must survive Gmail, a
//! `<style>` block only for dark mode and small screens. No webfonts are fetched. The logos are
//! absolute URLs on pigeonpost.dev (`site/email/`, versioned file names) and carry no
//! per-recipient data; there is no tracking pixel.

pub(crate) const LOGO_LIGHT: &str = "https://pigeonpost.dev/email/pigeonpost-logo-light.v1.png";
pub(crate) const LOGO_DARK: &str = "https://pigeonpost.dev/email/pigeonpost-logo-dark.v1.png";

pub(crate) const FONT: &str =
    "Geist,'Geist Sans',-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
pub(crate) const MONO: &str =
    "'Geist Mono',ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,'Liberation Mono',monospace";

/// Text colours (light mode; the `<style>` block remaps the classes for dark mode).
pub(crate) const INK: &str = "#14181f";
pub(crate) const BODY: &str = "#444c58";
pub(crate) const MUTED: &str = "#6b7480";
pub(crate) const RULE: &str = "#e6e9ee";
pub(crate) const SOFT: &str = "#f7f9fb";
pub(crate) const ACCENT: &str = "#2563eb";

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

/// A primary (filled) call-to-action button. `href` and `label` are plain text.
pub(crate) fn button(href: &str, label: &str) -> String {
    format!(
        r#"<a class="pp-btn" href="{href}" style="display:inline-block;background-color:{ACCENT};color:#ffffff;text-decoration:none;font-family:{FONT};font-weight:600;font-size:15px;line-height:20px;padding:12px 22px;border-radius:10px;margin:0 8px 8px 0;">{label}</a>"#,
        href = escape(href),
        label = escape(label),
    )
}

/// A secondary (outlined) button.
pub(crate) fn ghost(href: &str, label: &str) -> String {
    format!(
        r#"<a class="pp-ghost" href="{href}" style="display:inline-block;background-color:#ffffff;border:1px solid #d5dae1;color:{INK};text-decoration:none;font-family:{FONT};font-weight:600;font-size:14px;line-height:20px;padding:10px 18px;border-radius:10px;margin:0 8px 8px 0;">{label}</a>"#,
        href = escape(href),
        label = escape(label),
    )
}

/// Wraps rendered card content (already-escaped HTML) in the full document.
///
/// `preheader` is the inbox preview line (plain text). `footer` is already-escaped HTML shown
/// under the card in the muted style.
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
            "<div style=\"display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden;mso-hide:all;\">{}&#8199;&#65279;&#847;&#8199;&#65279;&#847;&#8199;&#65279;&#847;</div>\n",
            escape(preheader)
        )
    };
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
  a {{ color: {ACCENT}; text-decoration: none; }}
  @media (prefers-color-scheme: dark) {{
    .pp-bg {{ background-color: #0f1216 !important; }}
    .pp-card {{ background-color: #14181e !important; border-color: #2c313a !important; }}
    .pp-ink {{ color: #f4f6f9 !important; }}
    .pp-body {{ color: #c6ccd6 !important; }}
    .pp-muted, .pp-muted a {{ color: #949ba6 !important; }}
    .pp-rule {{ border-color: #2c313a !important; }}
    .pp-code, .pp-soft {{ background-color: #0f1216 !important; border-color: #454c58 !important; color: #f4f6f9 !important; }}
    .pp-btn {{ background-color: #3b82f6 !important; color: #ffffff !important; }}
    .pp-ghost {{ background-color: #14181e !important; border-color: #454c58 !important; color: #f4f6f9 !important; }}
    a.pp-link {{ color: #6c9bff !important; }}
    .pp-logo-light {{ display: none !important; }}
    .pp-logo-dark {{ display: block !important; max-height: none !important; overflow: visible !important; }}
  }}
  /* Outlook.com dark mode */
  [data-ogsc] .pp-ink {{ color: #f4f6f9 !important; }}
  [data-ogsc] .pp-body {{ color: #c6ccd6 !important; }}
  [data-ogsc] .pp-muted {{ color: #949ba6 !important; }}
  [data-ogsc] .pp-logo-light {{ display: none !important; }}
  [data-ogsc] .pp-logo-dark {{ display: block !important; max-height: none !important; overflow: visible !important; }}
  @media only screen and (max-width: 520px) {{
    .pp-card-pad {{ padding: 28px 22px !important; }}
    .pp-code {{ font-size: 34px !important; letter-spacing: 6px !important; padding-left: 18px !important; }}
  }}
</style>
</head>
<body class="pp-bg" style="margin:0;padding:0;background-color:{SOFT};">
{pre}<table role="presentation" class="pp-bg" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:{SOFT};">
  <tr>
    <td align="center" style="padding:32px 16px;">
      <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="max-width:520px;">
        <tr>
          <td style="padding:0 4px 20px;">
            <img class="pp-logo-light" src="{LOGO_LIGHT}" width="176" height="37" alt="Pigeonpost" style="display:block;border:0;outline:none;width:176px;height:37px;">
            <!--[if !mso]><!-->
            <div class="pp-logo-dark" style="display:none;max-height:0;overflow:hidden;mso-hide:all;">
              <img src="{LOGO_DARK}" width="176" height="37" alt="Pigeonpost" style="display:block;border:0;outline:none;width:176px;height:37px;">
            </div>
            <!--<![endif]-->
          </td>
        </tr>
        <tr>
          <td class="pp-card pp-card-pad pp-body" style="background-color:#ffffff;border:1px solid {RULE};border-radius:12px;padding:32px;font-family:{FONT};font-size:15px;line-height:1.6;color:{BODY};">
{card}
          </td>
        </tr>
        <tr>
          <td class="pp-muted" style="padding:20px 4px 0;font-family:{FONT};font-size:12px;line-height:1.6;color:{MUTED};">
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
        assert!(html.contains("width=\"176\" height=\"37\""));
        assert!(!html.contains("pigeonpost.dev/logo.png"));
        assert!(html.contains("<title>T &lt;x&gt;</title>"));
        assert!(html.contains("prefers-color-scheme: dark"));
        // Only the two logos are images: no pixel.
        assert_eq!(html.matches("<img").count(), 2);
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
