//! Restart-safe rate limiting (Phase 9 hardening).
//!
//! The limiters stay in memory, because they are consulted on hot paths (every org API request,
//! every group write). What used to be lost on every deploy — daily invitation and agent quotas,
//! hourly webhook and call-attempt limits, the org API's per-minute windows, and the set of spent
//! proof-of-work challenges — is now snapshotted into one SQLite row:
//!
//! - every [`SAVE_EVERY`] seconds while anything changed,
//! - immediately on SIGTERM/SIGINT, before the graceful shutdown begins (an open SSE stream can
//!   hold the shutdown until Docker kills the process, so the save cannot wait for it),
//!
//! and merged back before the listener accepts requests. A crash loses at most one interval. The
//! snapshot keeps only entries inside their window and is bounded by the limiters' own caps.

use crate::{now_unix, AppState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Seconds between snapshots.
pub(crate) const SAVE_EVERY: u64 = 10;

/// A snapshot older than this is ignored (every window it could hold has ended).
const MAX_AGE: u64 = crate::groups::LONGEST_RATE_WINDOW;

#[derive(Default, Serialize, Deserialize, PartialEq, Debug)]
pub(crate) struct Snapshot {
    #[serde(default)]
    general: HashMap<String, Vec<u64>>,
    #[serde(default)]
    org_api: Vec<(String, u64, u32, u32)>,
    #[serde(default)]
    pow_spent: Vec<(String, u64)>,
    #[serde(default)]
    call_pairs: Vec<(String, String, Vec<u64>)>,
}

pub(crate) fn capture(state: &AppState, now: u64) -> Snapshot {
    Snapshot {
        general: crate::groups::rate_limit_snapshot(now),
        org_api: crate::org_api::rate_snapshot(now),
        pow_spent: state.pow.spent_snapshot(now),
        call_pairs: state.call_pairs.snapshot(now),
    }
}

pub(crate) fn apply(state: &AppState, snap: Snapshot, now: u64) {
    crate::groups::rate_limit_restore(snap.general, now);
    crate::org_api::rate_restore(snap.org_api, now);
    state.pow.restore_spent(snap.pow_spent, now);
    state.call_pairs.restore(snap.call_pairs, now);
}

/// Load the last snapshot into the limiters. Call before serving.
pub(crate) async fn restore(state: &AppState) {
    let now = now_unix();
    match state.store.load_limiter_state().await {
        Ok(Some((saved_at, data))) if saved_at + MAX_AGE > now => {
            match serde_json::from_str::<Snapshot>(&data) {
                Ok(snap) => {
                    let keys = snap.general.len() + snap.org_api.len();
                    apply(state, snap, now);
                    tracing::info!(
                        keys,
                        age = now.saturating_sub(saved_at),
                        "rate limits restored"
                    );
                }
                Err(_) => tracing::warn!("rate-limit snapshot unreadable; starting empty"),
            }
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "rate-limit snapshot read failed"),
    }
}

/// Persist the current state. Returns the serialized form so the caller can skip identical saves.
pub(crate) async fn save(state: &AppState, last: Option<&str>) -> Option<String> {
    let now = now_unix();
    let data = match serde_json::to_string(&capture(state, now)) {
        Ok(d) => d,
        Err(_) => return None,
    };
    if last == Some(data.as_str()) {
        return Some(data);
    }
    match state.store.save_limiter_state(data.clone(), now).await {
        Ok(()) => Some(data),
        Err(e) => {
            tracing::error!(error = %e, "rate-limit snapshot write failed");
            None
        }
    }
}

/// Periodic and on-signal snapshots.
pub(crate) fn start(state: AppState) {
    let on_signal = state.clone();
    tokio::spawn(async move {
        crate::shutdown_signal().await;
        save(&on_signal, None).await;
        tracing::info!("rate limits saved for restart");
    });
    tokio::spawn(async move {
        let mut last: Option<String> = None;
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(SAVE_EVERY));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Some(d) = save(&state, last.as_deref()).await {
                last = Some(d);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quotas_survive_a_restart() {
        let state = crate::tests::test_state();
        let key = format!("limits-test:{}", crate::rand_hex(6));
        for _ in 0..3 {
            crate::groups::rate_limit(&key, 3, 86_400).unwrap();
        }
        assert!(crate::groups::rate_limit(&key, 3, 86_400).is_err());
        let challenge = state.pow.issue(8, now_unix());
        let solution = (0u64..)
            .map(|n| n.to_string())
            .find(|s| state.pow.verify(&challenge, s, now_unix()).is_ok())
            .unwrap();
        state
            .pow
            .consume(&challenge, &solution, now_unix())
            .unwrap();
        save(&state, None).await.unwrap();

        // A "restarted" process: fresh in-memory limiters, same database.
        let snap: Snapshot =
            serde_json::from_str(&state.store.load_limiter_state().await.unwrap().unwrap().1)
                .unwrap();
        assert!(snap.general.get(&key).is_some_and(|v| v.len() == 3));
        assert!(snap.pow_spent.iter().any(|(c, _)| *c == challenge));
        let fresh_state = || {
            let mut s = crate::tests::test_state();
            s.store = state.store.clone();
            s
        };
        // Without the snapshot a restarted process would accept the replay…
        assert!(fresh_state()
            .pow
            .consume(&challenge, &solution, now_unix())
            .is_ok());
        // …with it, it refuses.
        let fresh = fresh_state();
        restore(&fresh).await;
        assert_eq!(
            fresh.pow.consume(&challenge, &solution, now_unix()),
            Err(crate::pow::PowError::Replay)
        );
    }

    #[test]
    fn old_entries_are_dropped_on_restore() {
        let now = 1_000_000;
        let key = format!("limits-old:{}", crate::rand_hex(6));
        let mut general = HashMap::new();
        general.insert(key.clone(), vec![now - MAX_AGE - 5, now - 10]);
        crate::groups::rate_limit_restore(general, now);
        let snap = crate::groups::rate_limit_snapshot(now);
        assert_eq!(snap.get(&key), Some(&vec![now - 10]));
    }
}
