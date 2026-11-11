//! Verified publisher registration: the background work, every 30 s.
//!
//! - **Notices.** Every state change queues an email and a Pigeonpost message to the developer.
//!   Email goes through saas_be's `pigeonpost-notice` workflow (`notice_kind:
//!   publisher_application`) with the platform notification key (`POSTBOX_NOTIFY_URL`,
//!   `POSTBOX_NOTIFY_KEY`) to the application's contact email. The message comes from the platform
//!   marketplace mailbox (`POSTBOX_MARKETPLACE_MAILBOX`, default `/pigeonpost/marketplace`) to the
//!   application's support address when it is the developer's own mailbox, otherwise the
//!   applicant's first mailbox. Without configuration a notice is marked `skipped`.
//! - **Payee push.** After an approval the verified payee and tax profile goes to saas_be
//!   marketplace finance: `PUT {POSTBOX_SAAS_FINANCE_URL}/finance/publishers/{publisher}/payout-profile`
//!   with the tenant service token in `POSTBOX_SAAS_FINANCE_TOKEN_FILE` (scope
//!   `marketplace:finance`): the same upstream and credential as the back office's finance proxy.
//!   Not the billing bridge: the bridge is internet-facing and holds only `marketplace:bridge`.
//!   The payload is opened from the seals at send time and never stored or logged. saas_be keeps
//!   it `pending_verification` until finance verifies it before the first payout.
//! - **Malware scan.** Documents wait as `pending` until a scanner looks at them. With
//!   `POSTBOX_CLAMAV_ADDR` (`unix:/run/clamav/clamd.ctl` or `tcp:127.0.0.1:3310`) each one is
//!   streamed to clamd (`INSTREAM`); a hit marks it `infected` (never served, blocks approval).
//!   Without it they stay `pending` and staff see that.
//! - **Retention** (to be confirmed by legal; the values are configuration):
//!   rejected applications keep documents and sealed bank/tax details for
//!   `POSTBOX_PUBLISHER_DOCS_REJECTED_DAYS` (365); documents of a revoked publisher for
//!   `POSTBOX_PUBLISHER_DOCS_AFTER_END_DAYS` (3650, the Turkish commercial-records period);
//!   drafts untouched for `POSTBOX_PUBLISHER_DRAFT_DAYS` (180) lose their documents; a document
//!   withdrawn before the first submission is deleted at once. Approved publishers' records are
//!   kept while the publisher is certified. KYC images are never here; Shufti's retention applies.

use crate::groups::GResult;
use crate::publisher_registration::{app_in, open_sealed, App, Bank, Tax};
use crate::{now_unix, AppState};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

const MAX_ATTEMPTS: i64 = 10;

pub(crate) fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            work_once(&state).await;
        }
    });
}

pub(crate) async fn work_once(state: &AppState) {
    deliver_notices(state).await;
    push_payees(state).await;
    scan_documents(state).await;
    crate::publisher_kyc::poll_once(state).await;
    sweep_retention(state).await;
}

fn backoff(attempts: i64) -> i64 {
    60 * (1i64 << attempts.clamp(0, 8))
}

// ---- notices ------------------------------------------------------------------------------------------

/// Subject and lines for one state.
pub(crate) fn notice_text(
    state: &str,
    public_name: &str,
    message: Option<&str>,
) -> (String, Vec<String>) {
    let name = if public_name.is_empty() {
        "your publisher application".to_string()
    } else {
        format!("“{public_name}”")
    };
    let (subject, mut lines) = match state {
        "submitted" => (
            "We received your publisher application".to_string(),
            vec![format!(
                "Thank you. Pigeonpost staff will review {name}. Most reviews finish within five working days."
            )],
        ),
        "in_review" => (
            "Your publisher application is in review".to_string(),
            vec![format!("A Pigeonpost reviewer is checking {name} now.")],
        ),
        "needs_info" => (
            "Your publisher application needs more information".to_string(),
            vec![format!(
                "The reviewer needs more from you before {name} can be approved. Update the application and submit it again."
            )],
        ),
        "approved" => (
            "You are a verified Pigeonpost publisher".to_string(),
            vec![format!(
                "{name} is approved. You can now create templates and listings. Payout details are verified by finance before your first payout."
            )],
        ),
        "rejected" => (
            "Your publisher application was not approved".to_string(),
            vec![format!("{name} was not approved.")],
        ),
        _ => (
            "Your publisher application changed".to_string(),
            vec![format!("{name} is now {state}.")],
        ),
    };
    if let Some(m) = message {
        lines.push(format!("Message from Pigeonpost: {m}"));
    }
    lines.push(
        "Open your account to see the application: https://pigeonpost.dev/account#publish".into(),
    );
    (subject.replace(['\r', '\n'], " "), lines)
}

fn email(subject: &str, lines: &[String]) -> (String, String) {
    use crate::mail_layout::{self as layout, escape as esc, BODY, INK, MUTED};
    let body: String = lines
        .iter()
        .map(|l| {
            format!(
                r#"<p class="pp-body" style="margin:0 0 14px;color:{BODY};font-size:15px;line-height:1.6;">{}</p>"#,
                esc(l)
            )
        })
        .collect();
    let card = format!(
        r#"<p class="pp-muted" style="margin:0 0 6px;color:{MUTED};font-size:14px;line-height:1.5;">Publisher programme</p>
<h1 class="pp-ink" style="margin:0 0 16px;font-size:22px;line-height:1.3;font-weight:700;color:{INK};">{}</h1>
{body}<p style="margin:22px 0 6px;">{}</p>"#,
        esc(subject),
        layout::button(
            "https://pigeonpost.dev/account#publish",
            "Open the application"
        ),
    );
    let footer = "Pigeonpost · pigeonpost.dev<br>You received this because you applied to publish on the Pigeonpost marketplace.<br>Replies to this message are not read.";
    let html = layout::document("en", subject, &lines[0], &card, footer);
    let text = format!(
        "{}\n\n--\nPigeonpost · pigeonpost.dev\nReplies to this message are not read.\n",
        lines.join("\n\n")
    );
    (html, text)
}

/// The saas_be internal email request for one notice.
pub(crate) fn email_request(
    notice: &str,
    application: &str,
    to: &str,
    from: &str,
    subject: &str,
    lines: &[String],
) -> Value {
    let (html, text) = email(subject, lines);
    json!({
        "tenant_id": "pigeonpost",
        "workflow_id": "pigeonpost-notice",
        "transaction_id": notice,
        "to": to,
        "from": from,
        "sender_name": "Pigeonpost Marketplace",
        "subject": subject,
        "html": html,
        "text": text,
        "payload": {
            "tenant_id": "pigeonpost", "delivery_id": notice, "notice_kind": "publisher_application",
            "application_id": application, "subject": subject, "html": html, "text": text, "locale": "en",
        },
    })
}

struct Notice {
    id: String,
    app: App,
    channel: String,
    state: String,
    attempts: i64,
    message: Option<String>,
}

/// The developer's mailbox for a message: the support address when it is theirs, else the
/// applicant's first mailbox.
fn mailbox_for(c: &Connection, a: &App) -> rusqlite::Result<Option<String>> {
    let owners: Vec<String> = crate::publisher_registration::listeners_in(c, a)?;
    if let Some(addr) = a.profile.support_address.as_deref() {
        let found: Option<(String, Option<String>)> = c
            .query_row(
                "SELECT address, account_id FROM identities WHERE address = ?1 OR handle = ?1 LIMIT 1",
                [addr.to_ascii_lowercase()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((address, Some(account))) = found {
            if owners.contains(&account) {
                return Ok(Some(address));
            }
        }
    }
    c.query_row(
        "SELECT address FROM identities WHERE account_id = ?1 ORDER BY created_at, address LIMIT 1",
        [&a.applicant],
        |r| r.get(0),
    )
    .optional()
}

async fn deliver_notices(state: &AppState) {
    let now = now_unix() as i64;
    let due: Vec<Notice> = state
        .store
        .group_read(move |c| -> GResult<Vec<Notice>> {
            let mut stmt = c.prepare(
                "SELECT id, application_id, channel, state, attempts FROM publisher_application_notices
                  WHERE status = 'pending' AND next_at <= ?1 ORDER BY next_at LIMIT 20",
            )?;
            let rows: Vec<(String, String, String, String, i64)> = stmt
                .query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::new();
            for (id, app, channel, st, attempts) in rows {
                let Some(a) = app_in(c, &app)? else { continue };
                let message: Option<String> = c
                    .query_row(
                        "SELECT message FROM publisher_application_events
                          WHERE application_id = ?1 AND to_state = ?2 AND actor_role = 'staff'
                          ORDER BY created_at DESC, rowid DESC LIMIT 1",
                        params![app, st],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                out.push(Notice { id, app: a, channel, state: st, attempts, message });
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
    if due.is_empty() {
        return;
    }
    let mail = crate::org_notify::config();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok();
    for n in due {
        let (subject, lines) = notice_text(&n.state, &n.app.public_name, n.message.as_deref());
        let outcome: Result<(), String> = match n.channel.as_str() {
            "email" => match (&mail, &client, n.app.profile.contact_email.as_deref()) {
                (Some(cfg), Some(client), Some(to)) => {
                    let body = email_request(&n.id, &n.app.id, to, &cfg.from, &subject, &lines);
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
                _ => Err("skipped".into()),
            },
            _ => {
                let a = n.app.clone();
                let to = state
                    .store
                    .group_read(move |c| -> GResult<Option<String>> { Ok(mailbox_for(c, &a)?) })
                    .await
                    .ok()
                    .flatten();
                match (to, crate::marketplace_hosted::platform_mailbox(state).await) {
                    (Some(to), Ok(from)) => {
                        let body = format!("{subject}\n\n{}", lines.join("\n\n"));
                        let extras = crate::msgactions::SendExtras {
                            msg_ref: Some(crate::marketplace_hosted::msg_ref_for(
                                &n.id,
                                "publisher-notice",
                            )),
                            ..Default::default()
                        };
                        crate::do_send_with(
                            state,
                            &from,
                            &to,
                            &body,
                            crate::ThreadRequest::Default,
                            &extras,
                            false,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| e.code.to_string())
                    }
                    _ => Err("skipped".into()),
                }
            }
        };
        let (id, attempts) = (n.id.clone(), n.attempts + 1);
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                match &outcome {
                    Ok(()) => tx.execute(
                        "UPDATE publisher_application_notices SET status = 'sent', attempts = ?2, sent_at = ?3 WHERE id = ?1",
                        params![id, attempts, now],
                    )?,
                    Err(why) if why == "skipped" => tx.execute(
                        "UPDATE publisher_application_notices SET status = 'skipped', last_error = 'not configured' WHERE id = ?1",
                        [&id],
                    )?,
                    Err(why) => tx.execute(
                        "UPDATE publisher_application_notices SET attempts = ?2, last_error = ?3,
                            status = CASE WHEN ?2 >= ?4 THEN 'failed' ELSE 'pending' END, next_at = ?5
                          WHERE id = ?1",
                        params![id, attempts, why, MAX_ATTEMPTS, now + backoff(attempts)],
                    )?,
                };
                Ok(())
            })
            .await;
    }
}

// ---- payee push -----------------------------------------------------------------------------------------

/// saas_be marketplace finance for the payee push: `(base url, token)`, when configured.
fn finance() -> Option<(String, String)> {
    let url = std::env::var("POSTBOX_SAAS_FINANCE_URL")
        .ok()?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let token = std::fs::read_to_string(
        std::env::var("POSTBOX_SAAS_FINANCE_TOKEN_FILE")
            .ok()?
            .trim(),
    )
    .ok()?
    .trim()
    .to_string();
    (crate::nests::url_ok(&url) && token.len() >= 32).then_some((url, token))
}

/// The payout-profile body saas_be takes (`marketplace: international payee and tax profiles`).
pub(crate) fn payee_payload(
    a: &App,
    bank: &Bank,
    tax: &Tax,
    approved_by: &str,
    approved_at: i64,
    kyc_reference: Option<&str>,
) -> Value {
    let spec = crate::publisher_registration::type_spec(&a.account_type);
    json!({
        "actor_id": format!("staff:{approved_by}"),
        "beneficiary_name": bank.holder_name,
        "payee_type": if spec.is_some_and(|s| s.company || s.id == "sole_proprietor_tr") { "business" } else { "individual" },
        "method": bank.method,
        "iban": bank.iban,
        "account_number": bank.account_number,
        "swift_bic": bank.swift_bic,
        "routing_number": bank.routing_number,
        "ach_account_type": bank.ach_account_type,
        "bank_name": bank.bank_name,
        "bank_address": bank.bank_address,
        "country": bank.bank_country,
        "currency": bank.currency,
        "tax_id": tax.tax_id,
        "tax_profile": {
            "category": a.tax_category,
            "country": tax.tax_country,
            "treaty_country": tax.treaty_country,
            "residence_certificate_expires_at": tax.residence_certificate_expires_on,
        },
        "source": {
            "application_id": a.id,
            "approved_by": approved_by,
            "approved_at": crate::marketplace::iso(approved_at),
            "kyc_reference": kyc_reference,
        },
    })
}

async fn push_payees(state: &AppState) {
    let Some((url, token)) = finance() else {
        return;
    };
    let now = now_unix() as i64;
    #[allow(clippy::type_complexity)]
    let due: Vec<(String, String, String, String, i64, i64)> = state
        .store
        .group_read(move |c| -> GResult<_> {
            let mut stmt = c.prepare(
                "SELECT id, application_id, publisher_id, approved_by, attempts, created_at
                   FROM publisher_payee_pushes WHERE status = 'pending' AND next_at <= ?1
                  ORDER BY next_at LIMIT 10",
            )?;
            let rows = stmt
                .query_map([now], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return;
    };
    for (id, app, publisher, by, attempts, created) in due {
        let aid = app.clone();
        let loaded = state
            .store
            .group_read(move |c| -> GResult<(Option<App>, Option<String>)> {
                let reference: Option<String> = c
                    .query_row(
                        "SELECT reference FROM publisher_kyc_checks WHERE application_id = ?1
                            AND result = 'accepted' ORDER BY created_at DESC LIMIT 1",
                        [&aid],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok((app_in(c, &aid)?, reference))
            })
            .await;
        let outcome: Result<Option<i64>, (bool, String)> = match loaded {
            Ok((Some(a), reference)) => match (&a.bank_sealed, &a.tax_sealed) {
                (Some(b), Some(t)) => match (
                    open_sealed::<Bank>(state, &a.id, "bank", b),
                    open_sealed::<Tax>(state, &a.id, "tax", t),
                ) {
                    (Ok(bank), Ok(tax)) => {
                        let body =
                            payee_payload(&a, &bank, &tax, &by, created, reference.as_deref());
                        match client
                            .put(format!(
                                "{url}/finance/publishers/{publisher}/payout-profile"
                            ))
                            .bearer_auth(&token)
                            .json(&body)
                            .send()
                            .await
                        {
                            Ok(r) if r.status().is_success() => {
                                let v = r.json::<Value>().await.unwrap_or(Value::Null);
                                Ok(v["revision"].as_i64().or(v["data"]["revision"].as_i64()))
                            }
                            Ok(r) if r.status().is_client_error() => {
                                Err((true, format!("http {}", r.status().as_u16())))
                            }
                            Ok(r) => Err((false, format!("http {}", r.status().as_u16()))),
                            Err(_) => Err((false, "unreachable".into())),
                        }
                    }
                    _ => Err((true, "unsealable".into())),
                },
                _ => Err((true, "no payee details".into())),
            },
            Ok((None, _)) => Err((true, "no such application".into())),
            Err(_) => Err((false, "store".into())),
        };
        if let Err((_, why)) = &outcome {
            tracing::warn!(push = %id, %why, "publisher payee push failed");
        }
        let attempts = attempts + 1;
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                match &outcome {
                    Ok(rev) => tx.execute(
                        "UPDATE publisher_payee_pushes SET status = 'sent', attempts = ?2, sent_at = ?3,
                            remote_revision = ?4, last_error = NULL WHERE id = ?1",
                        params![id, attempts, now, rev],
                    )?,
                    Err((final_, why)) => tx.execute(
                        "UPDATE publisher_payee_pushes SET attempts = ?2, last_error = ?3,
                            status = CASE WHEN ?4 OR ?2 >= ?5 THEN 'failed' ELSE 'pending' END, next_at = ?6
                          WHERE id = ?1",
                        params![id, attempts, why, final_, MAX_ATTEMPTS, now + backoff(attempts)],
                    )?,
                };
                Ok(())
            })
            .await;
    }
}

// ---- malware scan -------------------------------------------------------------------------------------

/// Where clamd listens, from `POSTBOX_CLAMAV_ADDR`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Scanner {
    Unix(String),
    Tcp(String),
}

pub(crate) fn scanner() -> Option<Scanner> {
    let v = std::env::var("POSTBOX_CLAMAV_ADDR").ok()?;
    let v = v.trim();
    if let Some(p) = v.strip_prefix("unix:") {
        Some(Scanner::Unix(p.to_string()))
    } else {
        v.strip_prefix("tcp:").map(|a| Scanner::Tcp(a.to_string()))
    }
}

/// Parse clamd's `INSTREAM` reply: `Ok(None)` clean, `Ok(Some(signature))` infected.
pub(crate) fn clamd_verdict(reply: &str) -> Result<Option<String>, String> {
    let r = reply.trim_end_matches(['\0', '\n']).trim();
    let r = r.strip_prefix("stream:").map(str::trim).unwrap_or(r);
    if r == "OK" {
        Ok(None)
    } else if let Some(sig) = r.strip_suffix("FOUND") {
        Ok(Some(sig.trim().chars().take(120).collect()))
    } else {
        Err(r.chars().take(120).collect())
    }
}

/// Stream `bytes` to clamd with `zINSTREAM` and read its verdict.
pub(crate) async fn clamd_scan(s: &Scanner, bytes: &[u8]) -> Result<Option<String>, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn talk<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        mut io: T,
        bytes: &[u8],
    ) -> std::io::Result<String> {
        io.write_all(b"zINSTREAM\0").await?;
        for chunk in bytes.chunks(64 * 1024) {
            io.write_all(&(chunk.len() as u32).to_be_bytes()).await?;
            io.write_all(chunk).await?;
        }
        io.write_all(&0u32.to_be_bytes()).await?;
        io.flush().await?;
        let mut out = Vec::new();
        io.read_to_end(&mut out).await?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
    let fut = async {
        match s {
            #[cfg(unix)]
            Scanner::Unix(path) => talk(tokio::net::UnixStream::connect(path).await?, bytes).await,
            #[cfg(not(unix))]
            Scanner::Unix(_) => Err(std::io::Error::other("unix sockets unsupported")),
            Scanner::Tcp(addr) => talk(tokio::net::TcpStream::connect(addr).await?, bytes).await,
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(60), fut).await {
        Ok(Ok(reply)) => clamd_verdict(&reply),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timeout".into()),
    }
}

async fn scan_documents(state: &AppState) {
    let Some(s) = scanner() else { return };
    let Some(blobs) = crate::publisher_registration::doc_blobs(state) else {
        return;
    };
    let due: Vec<(String, Option<String>)> = state
        .store
        .group_read(|c| -> GResult<_> {
            let mut stmt = c.prepare(
                "SELECT id, blob FROM publisher_application_documents
                  WHERE scan_state IN ('pending','error') AND purged_at IS NULL
                    AND COALESCE(scanned_at, 0) < strftime('%s','now') - 600
                  ORDER BY created_at LIMIT 5",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for (id, blob) in due {
        let plain = blob
            .as_deref()
            .and_then(|b| blobs.get(b))
            .and_then(|sealed| {
                state
                    .vault
                    .open_labeled(
                        crate::publisher_registration::DOC_LABEL,
                        id.as_bytes(),
                        &sealed,
                    )
                    .ok()
            });
        let verdict = match plain {
            Some(p) => clamd_scan(&s, &p).await,
            None => Err("unreadable".into()),
        };
        let (st, detail) = match &verdict {
            Ok(None) => ("clean", None),
            Ok(Some(sig)) => ("infected", Some(sig.clone())),
            Err(e) => ("error", Some(e.clone())),
        };
        if st == "infected" {
            tracing::warn!(document = %id, "publisher document failed the malware scan");
        }
        let now = now_unix() as i64;
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE publisher_application_documents SET scan_state = ?2, scan_detail = ?3,
                        scanned_at = ?4 WHERE id = ?1",
                    params![id, st, detail, now],
                )?;
                Ok(())
            })
            .await;
    }
}

// ---- retention ------------------------------------------------------------------------------------------

async fn sweep_retention(state: &AppState) {
    let now = now_unix() as i64;
    let draft_days = crate::env_num::<i64>("POSTBOX_PUBLISHER_DRAFT_DAYS", 180);
    let rejected_days = crate::publisher_registration_staff::rejected_retention_days();
    let purged: Vec<String> = state
        .store
        .group_tx(move |tx| -> GResult<Vec<String>> {
            // Drafts abandoned long enough lose their documents.
            tx.execute(
                "UPDATE publisher_application_documents SET retain_until = ?1
                  WHERE retain_until IS NULL AND purged_at IS NULL AND application_id IN
                    (SELECT id FROM publisher_applications WHERE state = 'draft' AND updated_at < ?1 - ?2 * 86400)",
                params![now, draft_days],
            )?;
            let blobs: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT blob FROM publisher_application_documents
                      WHERE retain_until IS NOT NULL AND retain_until <= ?1 AND purged_at IS NULL
                        AND blob IS NOT NULL LIMIT 200",
                )?;
                let rows = stmt.query_map([now], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            tx.execute(
                "UPDATE publisher_application_documents SET purged_at = ?1, blob = NULL
                  WHERE retain_until IS NOT NULL AND retain_until <= ?1 AND purged_at IS NULL",
                [now],
            )?;
            // A rejected application's sealed bank and tax details go with its documents; the
            // masked copies stay as the record of what was reviewed.
            tx.execute(
                "UPDATE publisher_applications SET bank_sealed = NULL, tax_sealed = NULL
                  WHERE state = 'rejected' AND decided_at < ?1 - ?2 * 86400
                    AND (bank_sealed IS NOT NULL OR tax_sealed IS NOT NULL)",
                params![now, rejected_days],
            )?;
            Ok(blobs)
        })
        .await
        .unwrap_or_default();
    if let Some(store) = crate::publisher_registration::doc_blobs(state) {
        for b in purged {
            store.remove(&b);
        }
    }
}

/// When a publisher's certification ends (revocation), start the after-end retention clock on its
/// registration documents. In the caller's transaction.
pub(crate) fn certification_ended_in(
    c: &Connection,
    publisher: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let days = crate::env_num::<i64>("POSTBOX_PUBLISHER_DOCS_AFTER_END_DAYS", 3650);
    c.execute(
        "UPDATE publisher_application_documents SET retain_until = ?2
          WHERE retain_until IS NULL AND purged_at IS NULL AND application_id IN
            (SELECT id FROM publisher_applications WHERE publisher_id = ?1)",
        params![publisher, now + days * 86_400],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamd_replies() {
        assert_eq!(clamd_verdict("stream: OK\0"), Ok(None));
        assert_eq!(
            clamd_verdict("stream: Eicar-Test-Signature FOUND\0"),
            Ok(Some("Eicar-Test-Signature".into()))
        );
        assert!(clamd_verdict("INSTREAM size limit exceeded. ERROR\0").is_err());
    }

    #[test]
    fn notices_carry_the_staff_message() {
        let (subject, lines) = notice_text(
            "needs_info",
            "Acme",
            Some("Upload a newer faaliyet belgesi"),
        );
        assert!(subject.contains("needs more information"));
        assert!(lines.iter().any(|l| l.contains("newer faaliyet belgesi")));
        let req = email_request(
            "pan_1",
            "pap_1",
            "dev@example.com",
            "noreply@pigeonpost.dev",
            &subject,
            &lines,
        );
        let merged = crate::mail_layout::contract::merged(
            &req["payload"],
            "pigeonpost",
            req["subject"].as_str().unwrap(),
            req["html"].as_str().unwrap(),
            req["text"].as_str().unwrap(),
        );
        crate::mail_layout::contract::check("pigeonpost-notice", &merged).unwrap();
    }
}
