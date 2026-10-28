//! Disappearing messages (SDS §17) for direct threads, and the in-process expiry worker that
//! also runs group maintenance (`groups::sweep_in`).
//!
//! The clock starts when a message is sent (D-25). Expired rows are deleted every 30 s in batches
//! of 1,000, attachments are released, and affected accounts receive `message.expired`. Every
//! read path also filters on `expires_at`, so nothing is served in the gap between expiry and the
//! next pass. The reaper process runs the same sweep as a backstop.

use crate::capabilities;
use crate::filtering::feature_on;
use crate::groups::{self, GErr, GResult, TIMERS};
use crate::{now_unix, ApiError, AppState};
use axum::http::StatusCode;

/// How often the worker runs.
pub(crate) const SWEEP_SECS: u64 = 30;
/// Rows per pass.
pub(crate) const BATCH: usize = 1_000;

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
pub(crate) fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(d).map(Some)
}

/// The expiry for a message sent now in `thread_id` from `sender`'s side, if it has a timer.
pub(crate) async fn expiry_for(
    state: &AppState,
    sender: &crate::store::StoredIdentity,
    thread_id: &str,
    now: u64,
) -> Option<u64> {
    match state
        .store
        .thread_timer(sender.address.clone(), thread_id.to_string())
        .await
    {
        Ok(Some(Some(secs))) => Some(now + secs),
        _ => None,
    }
}

/// `PATCH /v1/threads/{id} {ephemeral_secs}` — either participant, both sides, one transaction.
pub(crate) async fn set_thread_timer(
    state: &AppState,
    me: &crate::store::StoredIdentity,
    thread_id: &str,
    secs: Option<u64>,
) -> Result<(), ApiError> {
    if !feature_on(state, me.account_id.as_deref(), capabilities::DISAPPEARING).await {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "disappearing messages are not enabled for this account",
        ));
    }
    if secs.is_some_and(|s| !TIMERS.contains(&s)) {
        return Err(ApiError::bad(
            "bad_request",
            "ephemeral_secs must be null, 300, 3600, 28800, 86400, 604800 or 2419200",
        ));
    }
    let changed_by = me.handle.clone().unwrap_or_else(|| me.address.clone());
    match state
        .store
        .set_thread_timer(
            me.address.clone(),
            thread_id.to_string(),
            secs,
            changed_by,
            now_unix(),
        )
        .await
    {
        Ok(Some(holders)) => {
            for holder in holders {
                if let Ok(Some(identity)) = state.store.get(holder).await {
                    if let Some(account) = identity.account_id.as_deref() {
                        state.event_hub.wake_mail(account);
                    }
                }
            }
            Ok(())
        }
        Ok(None) => Err(ApiError::bad(
            "unknown_thread",
            "no such thread in this mailbox",
        )),
        Err(e) => {
            tracing::error!(error = %e, "thread timer update failed");
            Err(ApiError::server("store_error"))
        }
    }
}

/// One expiry and maintenance pass: direct messages, then groups. Returns accounts to wake.
pub(crate) async fn sweep_once(
    store: &crate::store::Store,
    dm_blobs: Option<&crate::blobs::Blobs>,
    group_blobs: Option<&crate::blobs::Blobs>,
) -> Vec<String> {
    let now = now_unix();
    let mut wake = Vec::new();
    match store.expire_direct_messages(now, BATCH).await {
        Ok(done) => {
            if !done.expired.is_empty() {
                tracing::info!(
                    threads = done.expired.len(),
                    "disappearing direct messages expired"
                );
            }
            for (account, _, _, _) in &done.expired {
                if let Some(a) = account {
                    wake.push(a.clone());
                }
            }
            if let Some(blobs) = dm_blobs {
                for sha in &done.blobs {
                    blobs.remove(sha);
                }
            }
        }
        Err(e) => tracing::error!(error = %e, "direct message expiry failed"),
    }
    let swept: GResult<groups::Sweep> = store
        .group_tx(move |tx| groups::sweep_in(tx, now, BATCH))
        .await;
    // Live streams past four hours or no longer reported by the calls service (SDS §14A.5).
    match store
        .group_tx(move |tx| crate::streams::sweep_in(tx, now))
        .await
    {
        Ok(w) => wake.extend(w),
        Err(GErr { code, .. }) => tracing::error!(code, "stream maintenance failed"),
    }
    match swept {
        Ok(s) => {
            if s.expired_messages > 0 || s.purged_groups > 0 {
                tracing::info!(
                    expired = s.expired_messages,
                    purged_groups = s.purged_groups,
                    "group maintenance"
                );
            }
            if let Some(blobs) = group_blobs {
                for sha in &s.blobs {
                    blobs.remove(sha);
                }
            }
            wake.extend(s.wake);
        }
        Err(GErr { code, .. }) => tracing::error!(code, "group maintenance failed"),
    }
    wake
}

/// Run [`sweep_once`] every [`SWEEP_SECS`] inside the server.
pub(crate) fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(SWEEP_SECS));
        loop {
            ticker.tick().await;
            let group_blobs = groups::group_blobs(&state);
            let wake = sweep_once(&state.store, state.blobs.as_deref(), group_blobs.as_ref()).await;
            let mut seen = std::collections::HashSet::new();
            for account in wake {
                if seen.insert(account.clone()) {
                    state.event_hub.wake_mail(&account);
                }
            }
        }
    });
}
