//! Calendar side of a consented account deletion (SDS §14B, audit 2026-11-03 P2-17).
//!
//! `deploy/account-deletions.py --erase` removes the events a person organized outright. Before
//! that, the people they invited must hear that those meetings are off: contact-address guests get the
//! normal cancellation (with its `.ics` CANCEL), handle invitees get `event.cancelled`. The
//! operator runs `--cancel-calendar <request>` while the postbox is still up; it calls
//! `POST /internal/v1/account-deletions/{request_id}/cancel-calendar` (ops token), which cancels
//! every upcoming event the account organizes through the same path as an organizer's own
//! cancellation, so the delivery worker, the `calendar_email` flag, unsubscribes and the D-31
//! cancellation budget all apply unchanged. `--erase` then refuses while any of those notices is
//! still waiting to be sent.
//!
//! Idempotent: a second call cancels nothing new and reports what is still pending.

use super::events::{announce, retire_if_unused, write_ev};
use super::*;
use crate::internal_api::{authorize_service, Endpoint};
use crate::now_unix;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};

/// Outcome of one cancellation pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Cancelled {
    pub events: usize,
    /// Cancellation notices to contact addresses not yet handed to the mail provider.
    pub pending_notices: i64,
}

/// Cancel, inside `tx`, every upcoming event organized by `account` or one of its mailboxes.
pub(crate) fn cancel_organized_in(
    tx: &Connection,
    vault: &crate::vault::Vault,
    account: &str,
    fx: &mut Effects,
    now: u64,
) -> GResult<Cancelled> {
    let addresses: Vec<String> = {
        let mut stmt = tx.prepare("SELECT address FROM identities WHERE account_id = ?1")?;
        let rows = stmt
            .query_map(params![account], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        rows
    };
    let organized = |status: &str| -> GResult<Vec<String>> {
        let marks = vec!["?"; addresses.len()].join(",");
        let sql = format!(
            "SELECT id FROM events WHERE status = ?1 AND (span_to IS NULL OR span_to > ?2)
               AND (created_by_account = ?3{}) ORDER BY id",
            if addresses.is_empty() {
                String::new()
            } else {
                format!(" OR organizer IN ({marks})")
            }
        );
        let mut args: Vec<rusqlite::types::Value> = vec![
            status.to_string().into(),
            (now as i64).into(),
            account.to_string().into(),
        ];
        args.extend(addresses.iter().cloned().map(Into::into));
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    };
    let confirmed = organized("confirmed")?;
    for eid in &confirmed {
        let Some(mut ev) = load_ev(tx, vault, eid)? else {
            continue;
        };
        ev.status = "cancelled".into();
        ev.revision += 1;
        ev.sequence += 1;
        write_ev(tx, vault, &ev, false, now)?;
        if let Some(m) = &ev.meeting_id {
            retire_if_unused(tx, m, now)?;
        }
        announce(
            tx,
            &ev,
            "event.cancelled",
            None,
            Some("cancel"),
            Some(account),
            fx,
            now,
        )?;
    }
    let mut pending = 0i64;
    for eid in organized("cancelled")? {
        pending += tx.query_row(
            "SELECT COUNT(*) FROM calendar_delivery_outbox
              WHERE event_id = ?1 AND workflow = 'cancel' AND state IN ('queued','leased','uncertain')",
            params![eid],
            |r| r.get::<_, i64>(0),
        )?;
    }
    Ok(Cancelled {
        events: confirmed.len(),
        pending_notices: pending,
    })
}

fn valid_request_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("del_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `POST /internal/v1/account-deletions/{request_id}/cancel-calendar` (ops token).
pub(crate) async fn cancel_for_deletion(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    if let Err(resp) = authorize_service(
        &state,
        &headers,
        Endpoint::AccountErasure,
        Action::PrepareAccountErasure,
    ) {
        return *resp;
    }
    if !valid_request_id(&request_id) {
        return GErr::new(
            StatusCode::NOT_FOUND,
            "no_request",
            "no such deletion request",
        )
        .into_response();
    }
    let vault = state.vault.clone();
    let now = now_unix();
    let result = state
        .store
        .group_tx(move |tx| -> GResult<(Cancelled, Effects)> {
            let account: Option<String> = tx
                .query_row(
                    "SELECT account_id FROM account_deletion_requests
                      WHERE request_id = ?1 AND completed_at IS NULL",
                    params![request_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(account) = account else {
                return Err(GErr::new(
                    StatusCode::NOT_FOUND,
                    "no_request",
                    "no pending deletion request with this id",
                ));
            };
            let mut fx = Effects::default();
            let done = cancel_organized_in(tx, &vault, &account, &mut fx, now)?;
            Ok((done, fx))
        })
        .await;
    match result {
        Ok((done, fx)) => {
            tracing::info!(
                events = done.events,
                pending = done.pending_notices,
                "account deletion: organized events cancelled"
            );
            run_effects(&state, fx);
            axum::Json(json!({
                "events_cancelled": done.events,
                "pending_notices": done.pending_notices,
            }))
            .into_response()
        }
        Err(e) => e.into_response(),
    }
}
