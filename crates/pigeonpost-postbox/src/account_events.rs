//! `GET /v1/events` and the per-account wakeups behind it (SDS §3.5).
//!
//! # Compatibility
//!
//! `mail` events are byte-for-byte what they always were: `id:` is the message rowid, `data` is
//! `{created_at, event_id, mailbox, message_id, sender}`. Shipped clients parse exactly that — the
//! web inbox stores the id as a string, the Windows app parses it as an integer and ignores
//! anything else, agentd persists `event_id` from mail data only — and all of them ignore event
//! names they do not know.
//!
//! Typed events are added only when the `account_events` flag is on for the account. Their ids
//! (`"<mail_cursor>.<seq>"`) and data shape (`{event_id, type, mailbox, data}`) are deliberately
//! unparseable as a mail event by the integer-only clients, so they cannot move those clients'
//! cursors. See `store::events` for how the two kinds share one ordering.
//!
//! # Wakeups
//!
//! Each account with an open stream has one bounded `Notify`, registered while a stream is open
//! and dropped with the last one. A send wakes only the recipient's account; a flag change only
//! the account it names. The stream is pull-based — a slow consumer only delays its own next read
//! of the store — so nothing queues in memory per client. The global `inbox_signal` still releases
//! `GET /v1/inbox?wait=` long polls.

use crate::capabilities::ACCOUNT_EVENTS;
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::StreamCursor;
use crate::{bearer, now_unix, principal_for_token, ApiError, AppState, Principal};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// How many events one poll of the store will emit before yielding. A daemon reconnecting after a
/// long sleep should catch up in bounded chunks rather than building one enormous response.
const EVENT_BATCH: usize = 256;

/// How often a stream opened with an API key re-checks that the key still exists. A revoked key's
/// stream ends within this long (Phase 9; before, it lived until the client disconnected).
const CREDENTIAL_RECHECK: std::time::Duration =
    std::time::Duration::from_secs(if cfg!(test) { 1 } else { 60 });

/// Default ceiling on concurrently open streams per account.
const DEFAULT_STREAMS_PER_ACCOUNT: usize = 32;

/// Typed events are kept this long…
pub(crate) const EVENT_RETENTION_SECS: u64 = 7 * 86_400;
/// …and at most this many per account.
pub(crate) const EVENT_RETENTION_COUNT: usize = 10_000;

/// Per-account wakeups for open event streams.
pub(crate) struct EventHub {
    channels: Mutex<HashMap<String, (Arc<Notify>, usize)>>,
    max_per_account: usize,
}

impl Default for EventHub {
    fn default() -> Self {
        EventHub::new(DEFAULT_STREAMS_PER_ACCOUNT)
    }
}

impl EventHub {
    pub(crate) fn new(max_per_account: usize) -> Self {
        EventHub {
            channels: Mutex::new(HashMap::new()),
            max_per_account: max_per_account.max(1),
        }
    }

    pub(crate) fn from_env() -> Self {
        EventHub::new(
            std::env::var("POSTBOX_EVENT_STREAMS_PER_ACCOUNT")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_STREAMS_PER_ACCOUNT),
        )
    }

    /// Register one open stream for `account`, or `None` when it already holds the maximum.
    pub(crate) fn subscribe(self: &Arc<Self>, account: &str) -> Option<Subscription> {
        let mut channels = self.channels.lock().expect("event hub lock");
        let entry = channels
            .entry(account.to_string())
            .or_insert_with(|| (Arc::new(Notify::new()), 0));
        if entry.1 >= self.max_per_account {
            return None;
        }
        entry.1 += 1;
        Some(Subscription {
            hub: self.clone(),
            account: account.to_string(),
            notify: entry.0.clone(),
        })
    }

    /// Wake every open stream of `account`. A no-op when it has none.
    pub(crate) fn wake(&self, account: &str) {
        if let Some((notify, _)) = self.channels.lock().expect("event hub lock").get(account) {
            notify.notify_waiters();
        }
    }

    #[cfg(test)]
    pub(crate) fn open_accounts(&self) -> usize {
        self.channels.lock().expect("event hub lock").len()
    }
}

/// One open stream's registration; releases its slot when dropped.
pub(crate) struct Subscription {
    hub: Arc<EventHub>,
    account: String,
    notify: Arc<Notify>,
}

impl Subscription {
    pub(crate) fn notify(&self) -> &Notify {
        &self.notify
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut channels = self.hub.channels.lock().expect("event hub lock");
        if let Some(entry) = channels.get_mut(&self.account) {
            entry.1 = entry.1.saturating_sub(1);
            if entry.1 == 0 {
                channels.remove(&self.account);
            }
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct EventsQuery {
    /// Resume point. The `Last-Event-ID` header is the standard channel and is honoured too; this
    /// exists because not every client can set headers on an EventSource.
    #[serde(default)]
    last_event_id: Option<String>,
    /// `typed=1`: this client understands typed events (`"<mail>.<n>"` ids). New clients ask for
    /// them explicitly so the flag only governs legacy clients that never ask (Phase 3a).
    #[serde(default)]
    typed: Option<String>,
}

/// `GET /v1/events` — one Server-Sent Events stream per **account**.
///
/// Per account rather than per mailbox on purpose: a twenty-agent fleet holding a long poll each is
/// twenty sockets and twenty wakeups, while the thing that actually wants to know is the one daemon
/// on that machine. It learns *that* mail arrived and for which mailbox, then fetches the message
/// through the ordinary authenticated path.
///
/// Only metadata crosses this stream — no bodies. That keeps it cheap enough to hold open, and
/// means a stream leaking would disclose who wrote to whom rather than what they said.
///
/// `Last-Event-ID` is honoured. Without an id the stream starts at *now*, because a daemon starting
/// for the first time wants what happens next, not a replay of everything the account received.
pub(crate) async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Response {
    let principal = match principal_for_token(&state, bearer(&headers)).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let account = match &principal {
        Principal::Account(account, _) => account.clone(),
        Principal::Identity(_) => {
            return ApiError::bad(
                "use_api_key",
                "this endpoint needs an account API key, not a capability token",
            )
            .into_response()
        }
    };
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    if let Err(deny) = authorize(
        &Actor::Principal(&principal),
        Action::StreamAccountEvents,
        &Resource::Account(&account),
        &facts,
    ) {
        return deny.into_response();
    }

    let Some(subscription) = state.event_hub.subscribe(&account) else {
        return ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_streams",
            "this account already holds the maximum number of open event streams",
        )
        .with_retry_after(30)
        .into_response();
    };

    let resume = q
        .last_event_id
        .as_deref()
        .or_else(|| headers.get("last-event-id").and_then(|v| v.to_str().ok()));
    let start = match resume.and_then(StreamCursor::parse) {
        Some(cursor) => cursor,
        None => match state.store.stream_head(account.clone()).await {
            Ok(cursor) => cursor,
            Err(e) => {
                tracing::error!(error = %e, "events cursor read failed");
                return ApiError::server("store_error").into_response();
            }
        },
    };

    let asked_typed = matches!(q.typed.as_deref(), Some("1" | "true"));
    // Member sessions are short-lived JWTs; an API key lives until revoked, so its stream is
    // re-checked on a timer and closed once the key is gone.
    let key_hash = bearer(&headers)
        .filter(|t| crate::is_api_key(t))
        .map(|t| crate::sha256(t.as_bytes()));
    tracing::info!(%account, start = %start.typed_id(), "event stream opened");
    let stream = async_stream::stream! {
        let subscription = subscription;
        let mut cursor = start;
        let mut resync_sent = false;
        let mut checked_at = tokio::time::Instant::now();
        loop {
            if let Some(hash) = key_hash {
                if checked_at.elapsed() >= CREDENTIAL_RECHECK {
                    checked_at = tokio::time::Instant::now();
                    match state.store.account_for_key(hash).await {
                        Ok(Some(a)) if a == account => {}
                        Ok(_) => {
                            tracing::info!(%account, "event stream closed: API key revoked");
                            break;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "events credential recheck failed");
                            break;
                        }
                    }
                }
            }
            // Register interest before reading, for the same lost-wakeup reason `await_mail`
            // documents: an event landing between the read and the wait would notify nobody.
            let notified = subscription.notify().notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            // Re-read the account's mailboxes each pass, so a mailbox minted while the stream is
            // open starts producing events without the daemon reconnecting.
            let owners: Vec<String> = match state.store.list_by_account(account.clone()).await {
                Ok(rows) => rows.into_iter().map(|i| i.address).collect(),
                Err(e) => {
                    tracing::error!(error = %e, "events identity read failed");
                    break;
                }
            };
            let typed = match state
                .store
                .enabled_features(Some(account.clone()), vec![ACCOUNT_EVENTS])
                .await
            {
                Ok(on) => !on.is_empty() || asked_typed,
                Err(e) => {
                    tracing::error!(error = %e, "events flag read failed");
                    break;
                }
            };

            let batch = match state
                .store
                .stream_after(account.clone(), owners, cursor, typed, EVENT_BATCH)
                .await
            {
                Ok(batch) => batch,
                Err(e) => {
                    tracing::error!(error = %e, "events read failed");
                    break;
                }
            };

            if batch.resync && !resync_sent {
                // No id: the client's cursor stays where it was, so no mail is skipped. It
                // reloads authoritative state and carries on from here.
                resync_sent = true;
                let payload = json!({ "type": "resync_required", "reason": "cursor_expired" });
                yield Ok::<Event, std::convert::Infallible>(
                    Event::default().event("resync_required").data(payload.to_string()),
                );
            }

            let mut items: Vec<(StreamCursor, Event)> =
                Vec::with_capacity(batch.mail.len() + batch.typed.len());
            for (rowid, message) in batch.mail {
                let payload = json!({
                    "event_id": rowid,
                    "mailbox": message.owner,
                    "message_id": message.id,
                    "sender": message.sender,
                    "created_at": message.created_at,
                });
                items.push((
                    StreamCursor { mail: rowid, event: 0 },
                    Event::default().id(rowid.to_string()).event("mail").data(payload.to_string()),
                ));
            }
            // Group traffic is re-authorized at delivery (SDS §3.5): a mailbox removed after an
            // event was queued must not receive it.
            let parsed: Vec<serde_json::Value> = batch
                .typed
                .iter()
                .map(|e| serde_json::from_str(&e.data).unwrap_or(serde_json::Value::Null))
                .collect();
            let pairs: Vec<(String, String)> = batch
                .typed
                .iter()
                .zip(&parsed)
                .filter_map(|(e, d)| {
                    crate::groups::needs_membership(&e.kind, d)
                        .zip(e.mailbox.clone())
                })
                .collect();
            let allowed = if pairs.is_empty() {
                std::collections::HashSet::new()
            } else {
                let lookup = pairs.clone();
                match state
                    .store
                    .group_read(move |c| crate::groups::current_members(c, &lookup))
                    .await
                {
                    Ok(set) => set,
                    Err(crate::groups::GErr { code, .. }) => {
                        tracing::error!(code, "events membership check failed");
                        break;
                    }
                }
            };
            for (event, data) in batch.typed.into_iter().zip(parsed) {
                let id = event.cursor.typed_id();
                if let (Some(group), Some(mailbox)) = (
                    crate::groups::needs_membership(&event.kind, &data),
                    event.mailbox.clone(),
                ) {
                    if !allowed.contains(&(group, mailbox)) {
                        // Skipped, but the cursor still moves past it.
                        cursor = cursor.max(event.cursor);
                        continue;
                    }
                }
                let payload = json!({
                    "event_id": id,
                    "type": event.kind,
                    "mailbox": event.mailbox,
                    "data": data,
                });
                items.push((
                    event.cursor,
                    Event::default().id(id).event(event.kind).data(payload.to_string()),
                ));
            }
            items.sort_by_key(|(position, _)| *position);
            for (position, event) in items {
                cursor = position;
                yield Ok::<Event, std::convert::Infallible>(event);
            }

            if batch.more {
                continue;
            }
            match key_hash {
                Some(_) => {
                    let _ = tokio::time::timeout(CREDENTIAL_RECHECK, notified).await;
                }
                None => notified.await,
            }
        }
    };

    // The keep-alive is what makes this survive the middleboxes between a laptop and here: an idle
    // stream that sends nothing for minutes gets closed by something in between, and the daemon
    // would rediscover that only when mail failed to arrive.
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_are_bounded_per_account_and_released_on_drop() {
        let hub = Arc::new(EventHub::new(2));
        let a1 = hub.subscribe("a").unwrap();
        let a2 = hub.subscribe("a").unwrap();
        assert!(hub.subscribe("a").is_none(), "third stream refused");
        let b = hub.subscribe("b").unwrap();
        assert_eq!(hub.open_accounts(), 2);
        drop(a1);
        let a3 = hub.subscribe("a").unwrap();
        drop(a2);
        drop(a3);
        drop(b);
        assert_eq!(
            hub.open_accounts(),
            0,
            "no registry entry outlives its streams"
        );
    }

    #[tokio::test]
    async fn a_wake_reaches_only_the_named_account() {
        let hub = Arc::new(EventHub::new(4));
        let a = hub.subscribe("a").unwrap();
        let b = hub.subscribe("b").unwrap();
        let wa = a.notify().notified();
        let wb = b.notify().notified();
        tokio::pin!(wa);
        tokio::pin!(wb);
        wa.as_mut().enable();
        wb.as_mut().enable();
        hub.wake("a");
        hub.wake("nobody");
        tokio::time::timeout(std::time::Duration::from_millis(200), wa)
            .await
            .expect("a woken");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), wb)
                .await
                .is_err(),
            "b not woken"
        );
    }
}
