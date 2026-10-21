//! Org invitation delivery through saas_be's `pigeonpost-org-invite` workflow (SDS §7.2, §7.3).
//!
//! Deliveries are a durable outbox: the invite transaction records one row with the link sealed
//! under the vault; this worker sends it to `POST {POSTBOX_NOTIFY_URL}/v1/internal/notifications/email`
//! with the platform notification key (`POSTBOX_NOTIFY_KEY`), retries with backoff, and wipes the
//! sealed link once sent or abandoned. Retrying never creates a second reservation or token.
//!
//! Nothing is queued unless the `org_invite_email` flag is on; until then admins copy the link
//! that the invite response shows once. The message has no tracking pixel and a plain-text part.

use crate::groups::{GErr, GResult};
use crate::{now_unix, AppState};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

const MAX_ATTEMPTS: i64 = 8;

pub(crate) fn enqueue_in(
    c: &Connection,
    vault: &crate::vault::Vault,
    invite_id: &str,
    org_id: &str,
    link: &str,
    now: u64,
) -> GResult<()> {
    let sealed = vault.seal_bytes(link.as_bytes()).map_err(|_| {
        GErr::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "vault_error",
            "vault_error",
        )
    })?;
    // A resend cancels whatever was still waiting for this member.
    c.execute(
        "UPDATE org_invite_deliveries SET state = 'cancelled', link_sealed = x''
          WHERE state = 'pending' AND invite_id IN
            (SELECT id FROM org_invites WHERE member_id = (SELECT member_id FROM org_invites WHERE id = ?1) AND id <> ?1)",
        [invite_id],
    )?;
    c.execute(
        "INSERT INTO org_invite_deliveries (id, invite_id, org_id, link_sealed, state, next_at, created_at)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
        params![crate::org::new_id("dlv"), invite_id, org_id, sealed, now as i64],
    )?;
    Ok(())
}

struct Config {
    url: String,
    key: String,
    from: String,
}

fn config() -> Option<Config> {
    let url = std::env::var("POSTBOX_NOTIFY_URL")
        .ok()?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let key = std::env::var("POSTBOX_NOTIFY_KEY").ok()?.trim().to_string();
    if !url.starts_with("https://") && !url.starts_with("http://127.0.0.1") || key.is_empty() {
        return None;
    }
    let from =
        std::env::var("POSTBOX_NOTIFY_FROM").unwrap_or_else(|_| "invites@pigeonpost.dev".into());
    Some(Config { url, key, from })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Subject, HTML and text for one invitation.
pub(crate) fn render(
    inviter: &str,
    org: &str,
    handle: &str,
    link: &str,
    expires: &str,
) -> (String, String, String) {
    let subject =
        format!("{inviter} invited you to {org} on Pigeonpost").replace(['\r', '\n'], " ");
    let text = format!(
        "{inviter} invited you to join {org} on Pigeonpost.\n\nYour company address will be {handle}.\n\n\
         Accept the invitation: {link}\n\nThe link works once and expires on {expires}. It opens \
         enterprise.pigeonpost.dev — if it opens anywhere else, do not use it.\n\nIf you were not \
         expecting this, ignore it; nothing happens unless you accept.\n"
    );
    let html = format!(
        r#"<!doctype html><html><body style="margin:0;background:#f4f5f7;font-family:-apple-system,Segoe UI,Helvetica,Arial,sans-serif;color:#16181d">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0"><tr><td align="center" style="padding:32px 16px">
<table role="presentation" width="100%" style="max-width:520px;background:#ffffff;border-radius:14px;padding:32px" cellpadding="0" cellspacing="0">
<tr><td style="font-size:15px;font-weight:600;color:#3b5bdb;letter-spacing:.02em">Pigeonpost</td></tr>
<tr><td style="padding-top:20px;font-size:22px;font-weight:600;line-height:1.3">{inviter} invited you to {org}</td></tr>
<tr><td style="padding-top:12px;font-size:15px;line-height:1.55;color:#454a54">Your company address will be <strong style="color:#16181d">{handle}</strong>. You will use it in the Pigeonpost apps beside your own mailboxes.</td></tr>
<tr><td style="padding:28px 0"><a href="{link}" style="display:inline-block;background:#3b5bdb;color:#ffffff;text-decoration:none;font-weight:600;font-size:15px;padding:12px 22px;border-radius:10px">Accept invitation</a></td></tr>
<tr><td style="font-size:13px;line-height:1.5;color:#6b7280">This link works once and expires on {expires}. It opens enterprise.pigeonpost.dev — if your browser shows any other address, do not continue. If you were not expecting this invitation, you can ignore it.</td></tr>
</table></td></tr></table></body></html>"#,
        inviter = esc(inviter),
        org = esc(org),
        handle = esc(handle),
        link = esc(link),
        expires = esc(expires),
    );
    (subject, html, text)
}

fn date(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    // Civil-from-days (H. Hinnant), UTC.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{d} {} {y} (UTC)", MONTHS[(m - 1) as usize])
}

struct Due {
    id: String,
    invite_id: String,
    org_id: String,
    link_sealed: Vec<u8>,
    attempts: i64,
    contact_sealed: Option<Vec<u8>>,
    handle: Option<String>,
    org_name: String,
    inviter: String,
    expires_at: i64,
    live: bool,
}

/// Send what is due. Called by the org worker every 30 s.
pub(crate) async fn deliver_due(state: &AppState) {
    let Some(cfg) = config() else { return };
    let now = now_unix();
    let due: Vec<Due> = state
        .store
        .group_read(move |c: &Connection| -> GResult<Vec<Due>> {
            let mut stmt = c.prepare(
                "SELECT d.id, d.invite_id, d.org_id, d.link_sealed, d.attempts, m.contact_sealed, m.handle, o.name,
                        COALESCE((SELECT trim(first_name || ' ' || last_name) FROM org_members WHERE id = i.created_by), ''),
                        i.expires_at, (i.used_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?1)
                   FROM org_invite_deliveries d JOIN org_invites i ON i.id = d.invite_id
                   JOIN org_members m ON m.id = i.member_id JOIN orgs o ON o.id = d.org_id
                  WHERE d.state = 'pending' AND d.next_at <= ?1 ORDER BY d.next_at LIMIT 20",
            )?;
            let rows = stmt.query_map([now as i64], |r| {
                Ok(Due {
                    id: r.get(0)?,
                    invite_id: r.get(1)?,
                    org_id: r.get(2)?,
                    link_sealed: r.get(3)?,
                    attempts: r.get(4)?,
                    contact_sealed: r.get(5)?,
                    handle: r.get(6)?,
                    org_name: r.get(7)?,
                    inviter: r.get(8)?,
                    expires_at: r.get(9)?,
                    live: r.get(10)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap_or_default();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    for d in due {
        let opened = (
            state
                .vault
                .open_bytes(&d.link_sealed)
                .ok()
                .and_then(|b| String::from_utf8(b).ok()),
            d.contact_sealed
                .as_deref()
                .and_then(|s| state.vault.open_bytes(s).ok())
                .and_then(|b| String::from_utf8(b).ok()),
        );
        let outcome: Result<(), String> = match (d.live, opened) {
            (false, _) => Err("cancelled".into()),
            (true, (Some(link), Some(to))) => {
                let inviter = if d.inviter.is_empty() {
                    "An administrator".to_string()
                } else {
                    d.inviter.clone()
                };
                let handle = d.handle.clone().unwrap_or_default();
                let (subject, html, text) =
                    render(&inviter, &d.org_name, &handle, &link, &date(d.expires_at));
                let body = json!({
                    "tenant_id": "pigeonpost",
                    "workflow_id": "pigeonpost-org-invite",
                    "transaction_id": d.id,
                    "to": to,
                    "from": cfg.from,
                    "sender_name": "Pigeonpost",
                    "subject": subject,
                    "html": html,
                    "text": text,
                    "payload": {
                        "tenant_id": "pigeonpost", "delivery_id": d.id, "org_id": d.org_id, "invite_id": d.invite_id,
                        "subject": subject, "html": html, "text": text, "locale": "en",
                    },
                });
                match client
                    .post(format!("{}/v1/internal/notifications/email", cfg.url))
                    .header("x-api-key", &cfg.key)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => Ok(()),
                    Ok(r) => Err(format!("http {}", r.status().as_u16())),
                    Err(_) => Err("unreachable".into()),
                }
            }
            (true, _) => Err("unsealable".into()),
        };
        let id = d.id.clone();
        let attempts = d.attempts + 1;
        let recorded = outcome.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                match &recorded {
                    Ok(()) => {
                        tx.execute(
                            "UPDATE org_invite_deliveries SET state = 'sent', sent_at = ?2, attempts = ?3, link_sealed = x'' WHERE id = ?1",
                            params![id, now as i64, attempts],
                        )?;
                    }
                    Err(reason) => {
                        let final_ = reason == "cancelled" || attempts >= MAX_ATTEMPTS;
                        let backoff = 60i64 * (1 << attempts.min(8));
                        tx.execute(
                            "UPDATE org_invite_deliveries SET state = CASE WHEN ?4 THEN (CASE WHEN ?5 = 'cancelled' THEN 'cancelled' ELSE 'failed' END) ELSE 'pending' END,
                                    attempts = ?3, next_at = ?2 + ?6, last_error = ?5,
                                    link_sealed = CASE WHEN ?4 THEN x'' ELSE link_sealed END WHERE id = ?1",
                            params![id, now as i64, attempts, final_, reason, backoff],
                        )?;
                    }
                }
                Ok(())
            })
            .await;
        if let Err(reason) = &outcome {
            if reason != "cancelled" {
                tracing::warn!(delivery = %d.id, %reason, "org invitation delivery failed");
            }
        }
    }
}

/// The delivery state an admin may see for an invitation.
#[allow(dead_code)]
pub(crate) fn delivery_state_in(c: &Connection, invite_id: &str) -> GResult<Option<Value>> {
    Ok(c.query_row(
        "SELECT state, attempts, sent_at FROM org_invite_deliveries WHERE invite_id = ?1 ORDER BY created_at DESC LIMIT 1",
        [invite_id],
        |r| Ok(json!({ "state": r.get::<_, String>(0)?, "attempts": r.get::<_, i64>(1)?, "sent_at": r.get::<_, Option<i64>>(2)? })),
    )
    .optional()?)
}

/// Every background org task: deliveries (each tick), lifecycle (every 10 minutes), domain
/// re-checks (hourly).
pub(crate) fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick: u64 = 0;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            deliver_due(&state).await;
            if tick.is_multiple_of(20) {
                match state.store.org_lifecycle_sweep(now_unix()).await {
                    Ok(changed) => {
                        for (org, before, after) in changed {
                            tracing::info!(%org, %before, %after, "org lifecycle changed");
                            let now = now_unix();
                            let auditor = state.auditor.clone();
                            let _ = state
                                .store
                                .group_tx(move |tx| -> GResult<Vec<String>> {
                                    crate::org::audit_in(
                                        tx,
                                        &auditor,
                                        &org,
                                        crate::audit::ActorKind::System,
                                        None,
                                        "org.status",
                                        Some(&org),
                                        json!({ "from": before, "to": after }),
                                        None,
                                        now,
                                    )?;
                                    crate::org::emit_members(
                                        tx,
                                        &org,
                                        "org.status",
                                        &json!({ "org_id": org, "status": after }),
                                        now,
                                    )
                                })
                                .await
                                .map(|woke| woke.iter().for_each(|a| state.event_hub.wake(a)));
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "org lifecycle sweep failed"),
                }
            }
            if tick.is_multiple_of(120) {
                crate::org_routes::recheck_domains(&state).await;
            }
            tick = tick.wrapping_add(1);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_text_is_escaped_and_plain() {
        let (s, h, t) = render(
            "Jane <x>",
            "Acme & Co",
            "/acme/john",
            "https://enterprise.pigeonpost.dev/invite#tok",
            "1 Jan 2027 (UTC)",
        );
        assert_eq!(s, "Jane <x> invited you to Acme & Co on Pigeonpost");
        assert!(h.contains("Jane &lt;x&gt;") && h.contains("Acme &amp; Co"));
        assert!(!h.contains("<img"), "no tracking pixel");
        assert!(t.contains("https://enterprise.pigeonpost.dev/invite#tok"));
        assert_eq!(date(0), "1 Jan 1970 (UTC)");
        assert_eq!(date(1_798_761_600), "1 Jan 2027 (UTC)");
    }
}
