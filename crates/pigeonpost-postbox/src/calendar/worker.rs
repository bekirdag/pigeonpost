//! The calendar delivery worker (SDS §14B.12): a leased outbox for invitations, updates,
//! cancellations, verification codes and reminders, plus retention.
//!
//! Rows are claimed with a lease inside a short transaction; nothing holds the database while
//! talking to saas_be or push providers. Immediately before each send the row is re-judged against
//! current state (event and occurrence, recipient standing, answer, suppression, flags), so a
//! change made after queueing wins. Stable delivery ids (`dlv_…`, used as `transaction_id`) let a
//! retry be recognised downstream; exactly-once delivery is not claimed.

use super::mail::{self, Kind, Outgoing, SendResult};
use super::*;
use crate::now_unix;

/// How often the worker wakes without a nudge.
const TICK_SECS: u64 = 15;
/// A claimed row is someone else's to retry after this.
const LEASE_SECS: i64 = 120;
const MAX_ATTEMPTS: i64 = 8;
/// Reminders more than this late are dropped (SDS §14B.12).
const LATE_SECS: i64 = 15 * 60;
/// Contact-address messages still held (flag off) after this are dropped as stale.
const HOLD_MAX_SECS: i64 = 7 * 86_400;
/// Contact details are redacted this long after an event's last occurrence or cancellation.
const REDACT_AFTER_SECS: i64 = 30 * 86_400;

/// Queue a delivery (idempotent on its dedupe key).
#[allow(clippy::too_many_arguments)]
pub(crate) fn queue(
    c: &Connection,
    ev: &Ev,
    recipient_id: &str,
    workflow: &str,
    occurrence: Option<&str>,
    recipient_revision: i64,
    due_at: u64,
    account: Option<&str>,
    now: u64,
) -> GResult<()> {
    let dedupe = format!(
        "{workflow}:{}:{}:{}:{recipient_id}:{recipient_revision}",
        ev.id,
        ev.revision,
        occurrence.unwrap_or("*")
    );
    // A newer scheduling message to the same person makes older queued ones moot.
    if matches!(workflow, "update" | "cancel") {
        c.execute(
            "UPDATE calendar_delivery_outbox SET state = 'obsolete'
              WHERE event_id = ?1 AND recipient_id = ?2 AND state = 'queued'
                AND workflow IN ('update') AND COALESCE(occurrence_id, '*') = ?3",
            params![ev.id, recipient_id, occurrence.unwrap_or("*")],
        )?;
    }
    c.execute(
        "INSERT OR IGNORE INTO calendar_delivery_outbox (id, event_id, occurrence_id, recipient_id,
             workflow, event_revision, recipient_revision, dedupe_key, due_at, state, created_at,
             account_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?10, ?11)",
        params![
            new_id("dlv"),
            ev.id,
            occurrence,
            recipient_id,
            workflow,
            ev.revision,
            recipient_revision,
            dedupe,
            due_at as i64,
            now as i64,
            account.or(ev.created_by_account.as_deref())
        ],
    )?;
    Ok(())
}

/// Start the worker loop (server process only).
pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut last_retention = 0u64;
        loop {
            let now = now_unix();
            if let Err(e) = run_once(&state, now).await {
                tracing::warn!(code = e.code, "calendar worker pass failed");
            }
            if now >= last_retention + 3600 {
                last_retention = now;
                let vault = state.vault.clone();
                if let Err(e) = state
                    .store
                    .group_tx(move |tx| retention(tx, &vault, now))
                    .await
                {
                    tracing::warn!(code = e.code, "calendar retention failed");
                }
            }
            tokio::select! {
                _ = state.calendar.nudge.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)) => {}
            }
        }
    });
}

/// One pass: materialize due reminders, then claim and dispatch due rows.
pub(crate) async fn run_once(state: &AppState, now: u64) -> GResult<usize> {
    let vault = state.vault.clone();
    state
        .store
        .group_tx(move |tx| materialize_reminders(tx, &vault, now))
        .await?;
    let claimed: Vec<String> = state
        .store
        .group_tx(move |tx: &rusqlite::Transaction<'_>| -> GResult<Vec<String>> {
            tx.execute(
                "UPDATE calendar_delivery_outbox SET state = 'queued', lease_until = NULL
                  WHERE state = 'leased' AND lease_until < ?1",
                params![now as i64],
            )?;
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM calendar_delivery_outbox WHERE state = 'queued' AND due_at <= ?1
                      ORDER BY CASE workflow WHEN 'cancel' THEN 0 WHEN 'verify' THEN 0 ELSE 1 END, due_at
                      LIMIT 25",
                )?;
                let rows = stmt
                    .query_map(params![now as i64], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                rows
            };
            for id in &ids {
                tx.execute(
                    "UPDATE calendar_delivery_outbox SET state = 'leased', lease_until = ?2,
                            attempt_count = attempt_count + 1 WHERE id = ?1",
                    params![id, now as i64 + LEASE_SECS],
                )?;
            }
            Ok(ids)
        })
        .await?;
    let n = claimed.len();
    for id in claimed {
        dispatch(state, &id, now).await?;
    }
    Ok(n)
}

/// What to do with one claimed row.
enum Plan {
    Obsolete(&'static str),
    Suppressed,
    /// Leave queued until `until` (flag off or no transport configured).
    Hold(i64),
    Push {
        mailbox: String,
        note: push::Notification,
        wake: Vec<String>,
    },
    Send(Box<Outgoing>),
}

async fn dispatch(state: &AppState, id: &str, now: u64) -> GResult<()> {
    let vault = state.vault.clone();
    let rt = state.calendar.clone();
    let id_owned = id.to_string();
    // Is the organizer's account allowed to send to contact addresses? Read first (async flag API).
    let account: Option<String> = {
        let id = id_owned.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Option<String>> {
                Ok(c.query_row(
                    "SELECT COALESCE(o.account_id, e.created_by_account) FROM calendar_delivery_outbox o
                       JOIN events e ON e.id = o.event_id WHERE o.id = ?1",
                    params![id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
            })
            .await?
    };
    let external_on = feature_on(state, account.as_deref(), capabilities::CALENDAR_EMAIL).await
        && rt.notifier.configured();
    let plan = state
        .store
        .group_tx(move |tx: &rusqlite::Transaction<'_>| {
            plan_for(tx, &vault, &rt, &id_owned, now, external_on)
        })
        .await?;
    let (state_after, reference, error): (&str, Option<String>, Option<String>) = match plan {
        Plan::Obsolete(why) => ("obsolete", None, Some(why.to_string())),
        Plan::Suppressed => ("suppressed", None, None),
        Plan::Hold(until) => {
            let id = id.to_string();
            state
                .store
                .group_tx(move |tx: &rusqlite::Transaction<'_>| -> GResult<()> {
                    tx.execute(
                        "UPDATE calendar_delivery_outbox SET state = 'queued', lease_until = NULL,
                                due_at = ?2, attempt_count = MAX(attempt_count - 1, 0) WHERE id = ?1",
                        params![id, until],
                    )?;
                    Ok(())
                })
                .await?;
            return Ok(());
        }
        Plan::Push {
            mailbox,
            note,
            wake,
        } => {
            for a in wake {
                state.event_hub.wake(&a);
            }
            if state.apns.is_some() || state.fcm.is_some() {
                let s = state.clone();
                tokio::spawn(async move {
                    push::fan_out(
                        s.apns.clone(),
                        s.fcm.clone(),
                        s.store.clone(),
                        mailbox,
                        note,
                    )
                    .await;
                });
            }
            ("delivered", None, None)
        }
        Plan::Send(out) => match state.calendar.notifier.send(&out).await {
            SendResult::Accepted(r) => ("accepted", r, None),
            SendResult::Rejected(why) => ("failed", None, Some(why)),
            SendResult::Retry(why) => ("retry", None, Some(why)),
            SendResult::Uncertain => ("uncertain", None, Some("timeout".into())),
            SendResult::Unconfigured => ("retry", None, Some("unconfigured".into())),
        },
    };
    let id = id.to_string();
    let accounts = state
        .store
        .group_tx(move |tx: &rusqlite::Transaction<'_>| -> GResult<Vec<String>> {
            let attempts: i64 = tx.query_row(
                "SELECT attempt_count FROM calendar_delivery_outbox WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            let (st, due) = match state_after {
                "retry" | "uncertain" if attempts < MAX_ATTEMPTS => (
                    "queued",
                    now as i64 + (30i64 << attempts.min(7)).min(3600),
                ),
                "retry" | "uncertain" => ("failed", now as i64),
                other => (other, now as i64),
            };
            tx.execute(
                "UPDATE calendar_delivery_outbox SET state = ?2, lease_until = NULL, due_at = ?3,
                        provider_reference = COALESCE(?4, provider_reference),
                        delivered_at = CASE WHEN ?2 IN ('accepted','delivered') THEN ?5 ELSE delivered_at END,
                        last_error = ?6,
                        snapshot_sealed = CASE WHEN ?2 IN ('accepted','delivered','failed','obsolete','suppressed')
                                               THEN NULL ELSE snapshot_sealed END
                  WHERE id = ?1",
                params![id, st, due, reference, now as i64, error],
            )?;
            // The organizer's view shows delivery states: let their streams refresh.
            let organizer: Option<String> = tx
                .query_row(
                    "SELECT e.organizer FROM calendar_delivery_outbox o JOIN events e ON e.id = o.event_id
                      WHERE o.id = ?1 AND o.workflow IN ('invite','update')",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            let mut out = Vec::new();
            if let Some(o) = organizer {
                if let Some(a) = account_of(tx, &o)? {
                    out.push(a);
                }
            }
            Ok(out)
        })
        .await?;
    for a in accounts {
        state.event_hub.wake(&a);
    }
    Ok(())
}

struct Row {
    event_id: String,
    occurrence_id: Option<String>,
    recipient_id: String,
    workflow: String,
    reminder_id: Option<String>,
    due_at: i64,
    created_at: i64,
    snapshot: Option<Vec<u8>>,
}

fn plan_for(
    tx: &Connection,
    vault: &crate::vault::Vault,
    rt: &Runtime,
    id: &str,
    now: u64,
    external_on: bool,
) -> GResult<Plan> {
    let row = tx.query_row(
        "SELECT event_id, occurrence_id, recipient_id, workflow, reminder_id, due_at, created_at,
                snapshot_sealed
           FROM calendar_delivery_outbox WHERE id = ?1",
        params![id],
        |r| {
            Ok(Row {
                event_id: r.get(0)?,
                occurrence_id: r.get(1)?,
                recipient_id: r.get(2)?,
                workflow: r.get(3)?,
                reminder_id: r.get(4)?,
                due_at: r.get(5)?,
                created_at: r.get(6)?,
                snapshot: r.get(7)?,
            })
        },
    )?;
    let now_i = now as i64;
    let Some(ev) = load_ev(tx, vault, &row.event_id)? else {
        return Ok(Plan::Obsolete("event gone"));
    };
    let occ = match &row.occurrence_id {
        Some(o) => {
            let Some((_, key)) = parse_occurrence_id(o) else {
                return Ok(Plan::Obsolete("bad occurrence"));
            };
            occurrence_by_key(tx, vault, &ev, &key)?
        }
        None => None,
    };

    if row.workflow == "reminder" {
        return plan_push_reminder(tx, vault, &ev, occ, &row, now_i);
    }

    // Everything else goes to a contact address.
    let att = attendee(tx, &row.recipient_id)?;
    let Some(att) = att.filter(|a| a.kind == "external") else {
        return Ok(Plan::Obsolete("no such guest"));
    };
    if att.redacted_at.is_some() || att.contact_sealed.is_none() {
        return Ok(Plan::Obsolete("redacted"));
    }
    if !external_on {
        if now_i - row.created_at > HOLD_MAX_SECS {
            return Ok(Plan::Obsolete("held too long"));
        }
        return Ok(Plan::Hold(now_i + 3600));
    }
    let contact = open(
        vault,
        &format!("{}:contact", att.id),
        att.contact_sealed.as_ref().expect("checked"),
    );
    let hash = att.contact_hash.clone().unwrap_or_default();
    let organizer_name = super::ics::organizer_name(tx, &ev)?;

    if row.workflow == "verify" {
        let code = row
            .snapshot
            .as_ref()
            .map(|b| open(vault, &format!("{id}:code"), b))
            .unwrap_or_default();
        if code.is_empty() || now_i - row.created_at > 600 {
            return Ok(Plan::Obsolete("challenge expired"));
        }
        let view = mail::View {
            lang: ev.locale.clone(),
            code: Some(code),
            organizer: organizer_name.clone(),
            ..mail::View::default()
        };
        let (subject, html, text) = mail::render(Kind::Verify, &view);
        return Ok(Plan::Send(Box::new(Outgoing {
            to: contact,
            subject,
            html,
            text,
            workflow: "pigeonpost-meeting-verify",
            transaction_id: id.to_string(),
            sender_name: "Pigeonpost".into(),
            payload: json!({ "delivery_id": id, "challenge_id": row.reminder_id }),
            attachments: Vec::new(),
        })));
    }

    // Suppressions are checked immediately before every automatic message.
    let suppressed: bool = tx
        .query_row(
            "SELECT 1 FROM contact_suppressions WHERE contact_hash = ?1 AND scope IN ('all', ?2)",
            params![hash, ev.organizer],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if suppressed {
        return Ok(Plan::Suppressed);
    }

    let (kind, workflow, method) = match row.workflow.as_str() {
        "invite" => (Kind::Invite, "pigeonpost-meeting-invite", "REQUEST"),
        "update" => (Kind::Update, "pigeonpost-meeting-update", "REQUEST"),
        "cancel" => (Kind::Cancel, "pigeonpost-meeting-cancel", "CANCEL"),
        "reminder_external" => (Kind::Reminder, "pigeonpost-meeting-reminder", "REQUEST"),
        _ => return Ok(Plan::Obsolete("unknown workflow")),
    };
    if kind != Kind::Cancel {
        if att.removed_at.is_some() {
            return Ok(Plan::Obsolete("removed"));
        }
        if ev.status != "confirmed" {
            return Ok(Plan::Obsolete("cancelled"));
        }
        if let Some(o) = &occ {
            if o.status != "confirmed" || !standing(tx, &att, Some(&o.id))?.invited {
                return Ok(Plan::Obsolete("occurrence not current"));
            }
        }
        if ev.span_to.is_some_and(|end| end < now_i) {
            return Ok(Plan::Obsolete("event over"));
        }
    }
    if kind == Kind::Reminder {
        let Some(o) = &occ else {
            return Ok(Plan::Obsolete("no occurrence"));
        };
        if now_i > row.due_at + LATE_SECS || now_i > o.ends_at {
            return Ok(Plan::Obsolete("late"));
        }
        if standing(tx, &att, Some(&o.id))?.rsvp == "declined" {
            return Ok(Plan::Obsolete("declined"));
        }
    }

    // A one-occurrence guest only ever sees their occurrence.
    let only = if att.base != "invited" && occ.is_none() {
        let o: Option<String> = tx
            .query_row(
                "SELECT occurrence_id FROM event_occurrence_attendees
                  WHERE attendee_id = ?1 AND participation = 'invited' LIMIT 1",
                params![att.id],
                |r| r.get(0),
            )
            .optional()?;
        match o.and_then(|o| parse_occurrence_id(&o)) {
            Some((_, key)) => occurrence_by_key(tx, vault, &ev, &key)?,
            None => return Ok(Plan::Obsolete("no occurrence")),
        }
    } else {
        occ.clone()
    };

    // Fresh credentials for this message (the plaintext is never stored).
    let (token, _) = super::events::mint_invite_token(tx, &ev, &att, now)?;
    let unsub = new_secret();
    tx.execute(
        "INSERT INTO event_attendee_tokens (id, token_hash, attendee_id, purpose, audience,
             attendee_grant_version, created_at, expires_at)
         VALUES (?1, ?2, ?3, 'unsubscribe', 'calendar-unsubscribe', ?4, ?5, ?6)",
        params![
            new_id("tok"),
            secret_hash(&unsub),
            att.id,
            att.grant_version,
            now_i,
            now_i + 365 * 86_400
        ],
    )?;
    let inbox = &rt.inbox_url;
    let page = format!("{inbox}/m/{}#t={token}", ev.locator());
    let shown = only
        .clone()
        .or_else(|| ev.series().first().map(|i| effective(&ev, &i, None)));
    let when = match &shown {
        Some(o) if ev.all_day => mail::when_dates(&ev.locale, o.start_date(), o.end_date()),
        Some(o) => mail::when_timed(
            &ev.locale,
            o.starts_at,
            o.ends_at,
            o.tzid
                .as_deref()
                .and_then(|t| time::parse_tz(t).ok())
                .unwrap_or_else(|| ev.tz()),
        ),
        None => String::new(),
    };
    let cancel_message: Option<String> = if kind == Kind::Cancel {
        tx.query_row(
            "SELECT response FROM calendar_idempotency WHERE scope = ?1 AND action = 'cancel_note'
              ORDER BY created_at DESC LIMIT 1",
            params![ev.id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|b64| crate::b64_decode(&b64).ok())
        .and_then(|blob| {
            let rev: Option<i64> = tx
                .query_row(
                    "SELECT request_key FROM calendar_idempotency WHERE scope = ?1 AND action = 'cancel_note'
                      ORDER BY created_at DESC LIMIT 1",
                    params![ev.id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|s| s.parse().ok());
            rev.map(|rev| open(vault, &format!("{}:cancel-note:{rev}", ev.id), &blob))
        })
        .filter(|s| !s.is_empty())
    } else {
        None
    };
    let title = shown
        .as_ref()
        .map(|o| o.title.clone())
        .unwrap_or_else(|| ev.title.clone());
    let view = mail::View {
        lang: ev.locale.clone(),
        title,
        organizer: organizer_name.clone(),
        when,
        location: shown
            .as_ref()
            .map(|o| o.location.clone())
            .unwrap_or_default(),
        description: shown
            .as_ref()
            .map(|o| o.description.clone())
            .unwrap_or_default(),
        meeting_url: ev.meeting_url(inbox).map(|_| page.clone()),
        page_url: page.clone(),
        rsvp_urls: Some([
            format!("{page}&r=accepted"),
            format!("{page}&r=tentative"),
            format!("{page}&r=declined"),
        ]),
        unsubscribe_organizer: Some(format!("{inbox}/m/u#u={unsub}&s=organizer")),
        unsubscribe_all: Some(format!("{inbox}/m/u#u={unsub}&s=all")),
        cancel_message,
        minutes: occ
            .as_ref()
            .map(|o| (o.starts_at - row.due_at) / 60)
            .unwrap_or(0),
        recipient_name: att.display_name.clone().unwrap_or_default(),
        code: None,
    };
    let (subject, html, text) = mail::render(kind, &view);
    let mut attachments = Vec::new();
    if kind != Kind::Reminder {
        let cancel_whole = kind == Kind::Cancel && occ.is_none();
        let mut cancelled_ev = ev.clone();
        if cancel_whole {
            cancelled_ev.status = "cancelled".into();
        }
        let only_occ = if kind == Kind::Cancel {
            occ.clone().map(|mut o| {
                o.status = "cancelled".into();
                o
            })
        } else {
            only.clone()
        };
        let overrides = if only_occ.is_none() {
            super::ics::override_occs(tx, vault, &ev)?
        } else {
            Vec::new()
        };
        let doc = super::ics::Doc {
            ev: &cancelled_ev,
            method,
            recipient: Some(super::ics::Recipient {
                name: att.display_name.clone().unwrap_or_else(|| "Guest".into()),
                uri: format!("mailto:{contact}"),
                partstat: super::ics::partstat(&att.rsvp),
            }),
            only: only_occ.as_ref(),
            overrides,
            organizer_name: organizer_name.clone(),
            inbox,
            busy_only: false,
            no_description: false,
            now: now_i,
        };
        let text_ics = super::ics::render(&doc);
        attachments.push((
            "invite.ics".to_string(),
            format!("text/calendar; charset=utf-8; method={method}"),
            text_ics.into_bytes(),
        ));
    }
    let mut payload = json!({
        "delivery_id": id, "event_id": ev.id, "sequence": ev.sequence.max(0),
        "organizer_handle": organizer_name, "ics_method": method,
    });
    if let Some(o) = &occ {
        payload["occurrence_id"] = json!(o.id);
    }
    if kind == Kind::Reminder {
        payload["reminder_id"] = json!(row.reminder_id);
    }
    Ok(Plan::Send(Box::new(Outgoing {
        to: contact,
        subject,
        html,
        text,
        workflow,
        transaction_id: id.to_string(),
        sender_name: mail::sender_name(&organizer_name),
        payload,
        attachments,
    })))
}

fn plan_push_reminder(
    tx: &Connection,
    vault: &crate::vault::Vault,
    ev: &Ev,
    occ: Option<Occ>,
    row: &Row,
    now: i64,
) -> GResult<Plan> {
    let _ = vault;
    let Some(occ) = occ else {
        return Ok(Plan::Obsolete("no occurrence"));
    };
    if ev.status != "confirmed" || occ.status != "confirmed" {
        return Ok(Plan::Obsolete("cancelled"));
    }
    let Some(rem_id) = &row.reminder_id else {
        return Ok(Plan::Obsolete("no reminder"));
    };
    let rem: Option<(i64, bool)> = tx
        .query_row(
            "SELECT minutes_before, enabled FROM event_reminders WHERE id = ?1",
            params![rem_id],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    let Some((minutes, true)) = rem else {
        return Ok(Plan::Obsolete("reminder changed"));
    };
    if occ.starts_at - minutes * 60 != row.due_at {
        return Ok(Plan::Obsolete("moved"));
    }
    if now > row.due_at + LATE_SECS || now > occ.ends_at {
        return Ok(Plan::Obsolete("late"));
    }
    let mailbox = if let Some(owner) = row.recipient_id.strip_prefix("owner:") {
        if owner != ev.organizer {
            return Ok(Plan::Obsolete("not organizer"));
        }
        owner.to_string()
    } else {
        let Some(att) = attendee(tx, &row.recipient_id)? else {
            return Ok(Plan::Obsolete("gone"));
        };
        if att.removed_at.is_some() || att.suppressed || att.filtered || att.kind != "handle" {
            return Ok(Plan::Obsolete("not eligible"));
        }
        let s = standing(tx, &att, Some(&occ.id))?;
        if !s.invited || s.rsvp == "declined" {
            return Ok(Plan::Obsolete("not eligible"));
        }
        att.address.clone().unwrap_or_default()
    };
    let previews: bool = tx
        .query_row(
            "SELECT previews FROM calendar_settings WHERE mailbox = ?1",
            params![mailbox],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        != 0;
    let lead = if minutes == 0 {
        "Starting now".to_string()
    } else {
        format!("Starting in {minutes} minutes")
    };
    let body = if previews {
        format!("{}: {lead}", occ.title)
    } else {
        format!("Event: {lead}")
    };
    let mut fx = Effects::default();
    fx.event(
        tx,
        &mailbox,
        "event.reminder",
        json!({ "event_id": ev.id, "occurrence_id": occ.id, "reminder_id": rem_id, "starts_at": occ.starts_at }),
        now as u64,
    )?;
    Ok(Plan::Push {
        wake: fx.wake,
        mailbox: mailbox.clone(),
        note: push::Notification {
            title: "Calendar".into(),
            subtitle: mailbox,
            body,
            message_id: format!("cal:{}:{rem_id}", occ.id),
            peer: ev.organizer.clone(),
            unread: 0,
        },
    })
}

/// Queue reminder deliveries for occurrences starting soon. Bounded: only reminders of live
/// events whose span reaches the next four weeks, and only occurrences whose reminder time falls
/// in `[now − 15 min, now + 2 min]`.
#[allow(clippy::type_complexity)]
fn materialize_reminders(
    tx: &rusqlite::Transaction<'_>,
    vault: &crate::vault::Vault,
    now: u64,
) -> GResult<()> {
    let now = now as i64;
    let rows: Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        String,
        i64,
    )> = {
        let mut stmt = tx.prepare(
            "SELECT r.id, r.event_id, r.attendee_id, r.owner_identity, r.minutes_before, r.channel, r.revision
               FROM event_reminders r JOIN events e ON e.id = r.event_id
              WHERE r.enabled = 1 AND e.status = 'confirmed'
                AND e.span_from < ?1 + 40320 * 60 + 3600 AND (e.span_to IS NULL OR e.span_to > ?1)",
        )?;
        let rows = stmt
            .query_map(params![now], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut cache: HashMap<String, Option<Ev>> = HashMap::new();
    for (rid, eid, att, owner, minutes, channel, rev) in rows {
        let ev = match cache.get(&eid) {
            Some(e) => e.clone(),
            None => {
                let e = load_ev(tx, vault, &eid)?;
                cache.insert(eid.clone(), e.clone());
                e
            }
        };
        let Some(ev) = ev else { continue };
        let lead = minutes * 60;
        let (from, to) = (now - LATE_SECS + lead, now + 120 + lead);
        let Ok(occs) = occurrences_in(tx, vault, &ev, from, to + 1, 200) else {
            continue;
        };
        for occ in occs {
            if occ.status != "confirmed" || occ.starts_at < from || occ.starts_at > to || ev.all_day
            {
                continue;
            }
            let recipient = match (&att, &owner) {
                (Some(a), _) => a.clone(),
                (None, Some(o)) => format!("owner:{o}"),
                _ => continue,
            };
            let workflow = if channel == "external" {
                "reminder_external"
            } else {
                "reminder"
            };
            let dedupe = format!("rem:{rid}:{rev}:{}:{}", occ.id, occ.starts_at);
            tx.execute(
                "INSERT OR IGNORE INTO calendar_delivery_outbox (id, event_id, occurrence_id, recipient_id,
                     workflow, event_revision, recipient_revision, reminder_id, reminder_revision,
                     dedupe_key, due_at, state, created_at, account_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, 'queued', ?11, ?12)",
                params![
                    new_id("dlv"),
                    ev.id,
                    occ.id,
                    recipient,
                    workflow,
                    ev.revision,
                    rid,
                    rev,
                    dedupe,
                    occ.starts_at - lead,
                    now,
                    ev.created_by_account
                ],
            )?;
        }
    }
    Ok(())
}

/// Retention (SDS §14B.8): redact contact details 30 days after an event's last occurrence,
/// cancellation or the guest's removal; drop expired credentials, old idempotency records and
/// drafts; keep only keyed suppression records.
pub(crate) fn retention(
    tx: &rusqlite::Transaction<'_>,
    _vault: &crate::vault::Vault,
    now: u64,
) -> GResult<()> {
    let now = now as i64;
    let cutoff = now - REDACT_AFTER_SECS;
    tx.execute(
        "UPDATE event_attendees SET contact_sealed = NULL, display_name = NULL, rsvp_note_sealed = NULL,
                redacted_at = ?1, grant_version = grant_version + 1
          WHERE kind = 'external' AND redacted_at IS NULL
            AND (removed_at < ?2
                 OR event_id IN (SELECT id FROM events
                                  WHERE (span_to IS NOT NULL AND span_to < ?2)
                                     OR (status = 'cancelled' AND updated_at < ?2)))",
        params![now, cutoff],
    )?;
    tx.execute(
        "UPDATE event_attendees SET rsvp_note_sealed = NULL
          WHERE rsvp_note_sealed IS NOT NULL AND event_id IN
                (SELECT id FROM events WHERE (span_to IS NOT NULL AND span_to < ?1)
                                          OR (status = 'cancelled' AND updated_at < ?1))",
        params![cutoff],
    )?;
    tx.execute(
        "DELETE FROM event_attendee_tokens WHERE purpose = 'invite_manage' AND expires_at < ?1",
        params![now - 7 * 86_400],
    )?;
    tx.execute(
        "DELETE FROM event_attendee_tokens WHERE purpose = 'unsubscribe' AND expires_at < ?1",
        params![now],
    )?;
    tx.execute(
        "DELETE FROM event_guest_challenges WHERE expires_at < ?1",
        params![now - 86_400],
    )?;
    tx.execute(
        "DELETE FROM event_join_tickets WHERE expires_at < ?1",
        params![now - 86_400],
    )?;
    tx.execute(
        "DELETE FROM event_guest_sessions WHERE expires_at < ?1",
        params![now - 7 * 86_400],
    )?;
    tx.execute(
        "UPDATE calendar_delivery_outbox SET snapshot_sealed = NULL WHERE snapshot_sealed IS NOT NULL AND created_at < ?1",
        params![now - 86_400],
    )?;
    tx.execute(
        "DELETE FROM calendar_delivery_outbox WHERE state IN ('accepted','delivered','failed','obsolete','suppressed')
            AND created_at < ?1",
        params![now - 90 * 86_400],
    )?;
    tx.execute(
        "DELETE FROM calendar_idempotency WHERE created_at < ?1 AND action != 'cancel_note'",
        params![now - IDEMPOTENCY_SECS as i64],
    )?;
    tx.execute(
        "DELETE FROM calendar_idempotency WHERE action = 'cancel_note' AND created_at < ?1",
        params![cutoff],
    )?;
    tx.execute(
        "DELETE FROM calendar_import_drafts WHERE expires_at < ?1",
        params![now],
    )?;
    Ok(())
}
