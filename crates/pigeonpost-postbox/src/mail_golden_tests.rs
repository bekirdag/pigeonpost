//! Golden files for every message the postbox renders to a contact address: each calendar kind
//! (invitation, update, cancellation, reminder, guest code) in English and Turkish, and the
//! organization invitation. Subject, HTML and plain text are pinned byte for byte in
//! `tests/mail-golden/`, so any change to the design kit frame or the wording shows up as a diff.
//!
//! Regenerate after an intended change with
//! `UPDATE_MAIL_GOLDEN=1 cargo test -p pigeonpost-postbox mail_golden`, then review the diff
//! (the `.html` files open directly in a browser).

use crate::calendar::mail::{self, Kind, View};
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mail-golden")
}

fn check(name: &str, (subject, html, text): (String, String, String)) {
    let files = [
        (format!("{name}.subject.txt"), format!("{subject}\n")),
        (format!("{name}.html"), html),
        (format!("{name}.txt"), text),
    ];
    let update = std::env::var("UPDATE_MAIL_GOLDEN").is_ok_and(|v| v == "1");
    for (file, got) in files {
        let path = dir().join(&file);
        if update {
            std::fs::create_dir_all(dir()).expect("golden dir");
            std::fs::write(&path, &got).expect("write golden");
            continue;
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| {
                panic!("missing golden {file}; run with UPDATE_MAIL_GOLDEN=1 and review it")
            })
            // `.gitattributes` keeps these LF; tolerate a checkout that converted them anyway.
            .replace("\r\n", "\n");
        assert!(
            want == got,
            "{file} changed; run with UPDATE_MAIL_GOLDEN=1 and review the diff"
        );
    }
}

/// A fixed view with hostile text in every free field.
fn view(lang: &str) -> View {
    View {
        lang: lang.into(),
        title: "Design <review> & \"plan\"".into(),
        organizer: "Çağrı Dağ".into(),
        when: mail::when_timed(
            lang,
            1_792_735_200,
            1_792_738_800,
            "Europe/Istanbul".parse().unwrap(),
        ),
        location: "Room 2 <b>north</b>".into(),
        description: "Agenda:\n1. Numbers\n2. See https://evil.example/x?a=1&b=<2>".into(),
        meeting_url: Some("https://inbox.pigeonpost.dev/m/ABCDEFGHJK".into()),
        page_url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=tok".into(),
        rsvp_urls: Some([
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=tok&r=yes".into(),
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=tok&r=maybe".into(),
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=tok&r=no".into(),
        ]),
        unsubscribe_organizer: Some("https://inbox.pigeonpost.dev/m/u#u=a&s=organizer".into()),
        unsubscribe_all: Some("https://inbox.pigeonpost.dev/m/u#u=a&s=all".into()),
        ics_url: Some("https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=tok&a=ics".into()),
        minutes: 10,
        ..View::default()
    }
}

fn kinds() -> [(&'static str, Kind); 5] {
    [
        ("invite", Kind::Invite),
        ("update", Kind::Update),
        ("cancel", Kind::Cancel),
        ("reminder", Kind::Reminder),
        ("verify", Kind::Verify),
    ]
}

fn calendar_view(lang: &str, kind: Kind) -> View {
    let mut v = view(lang);
    match kind {
        Kind::Cancel => {
            v.cancel_message = Some(if lang == "tr" {
                "Bu hafta yapamıyoruz <üzgünüm>.\nYeni tarih yakında.".into()
            } else {
                "We can't make it this week <sorry>.\nNew date soon.".into()
            });
        }
        Kind::Reminder => {
            v.rsvp_urls = None;
            v.ics_url = None;
        }
        Kind::Verify => {
            v = View {
                lang: lang.into(),
                code: Some("482915".into()),
                organizer: "Çağrı Dağ".into(),
                ..View::default()
            };
        }
        _ => {}
    }
    v
}

#[test]
fn mail_golden_calendar_messages_in_both_languages() {
    for lang in ["en", "tr"] {
        for (name, kind) in kinds() {
            check(
                &format!("calendar-{name}.{lang}"),
                mail::render(kind, &calendar_view(lang, kind)),
            );
        }
    }
}

#[test]
fn mail_golden_organization_invitation() {
    check(
        "org-invite.en",
        crate::org_notify::render(
            "Jane <x> Doe",
            "Acme & Co",
            "/acme/jane",
            "https://enterprise.pigeonpost.dev/invite#tok&x=1",
            "22 Nov 2026 (UTC)",
        ),
    );
}

/// The guarantees every golden shares, checked on the rendered output (not on the files).
#[test]
fn mail_golden_messages_share_the_kit_frame_and_guarantees() {
    let mut all = Vec::new();
    for lang in ["en", "tr"] {
        for (name, kind) in kinds() {
            all.push((
                format!("{name}.{lang}"),
                mail::render(kind, &calendar_view(lang, kind)),
            ));
        }
    }
    all.push((
        "org".into(),
        crate::org_notify::render(
            "J",
            "A",
            "/a/j",
            "https://enterprise.pigeonpost.dev/i#t",
            "x",
        ),
    ));
    for (name, (subject, html, text)) in &all {
        // The kit frame.
        assert!(html.contains("max-width:560px"), "{name}");
        assert!(html.contains("class=\"pp-brand\""), "{name}");
        assert!(html.contains(crate::mail_layout::LOGO_LIGHT), "{name}");
        assert!(html.contains(crate::mail_layout::LOGO_DARK), "{name}");
        assert!(html.contains("prefers-color-scheme: dark"), "{name}");
        assert!(html.contains(crate::mail_layout::FOOTER_BRAND), "{name}");
        // No pixel, no script, no webfont.
        assert_eq!(html.matches("<img").count(), 2, "{name}");
        assert!(
            !html.contains("<script") && !html.contains("@font-face"),
            "{name}"
        );
        // Hostile text never reaches the markup unescaped.
        assert!(
            !html.contains("<review>") && !html.contains("<b>north"),
            "{name}"
        );
        assert!(
            !html.contains("<sorry>") && !html.contains("<üzgünüm>"),
            "{name}"
        );
        assert!(!html.contains("Jane <x>"), "{name}");
        // Descriptions and messages are never linked.
        assert!(!html.contains("href=\"https://evil.example"), "{name}");
        // A plain-text part, and a single-line subject.
        assert!(
            !text.is_empty() && !text.contains("<p") && !text.contains("<table"),
            "{name}"
        );
        assert!(!subject.contains(['\r', '\n']), "{name}");
    }
}
