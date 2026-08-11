//! Supervised, bounded retention sweeping.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use crate::error::{LoftError, Result};
use crate::store::LoftStore;

/// Delete every currently expired row in bounded transactions. SQLite work runs only on the
/// blocking pool and errors are returned to the supervisor instead of being logged and forgotten.
pub async fn sweep_once(store: &Arc<dyn LoftStore>, batch: usize) -> Result<usize> {
    if batch == 0 {
        return Err(LoftError::Configuration("retention batch must be nonzero"));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut total = 0usize;
    loop {
        let store = Arc::clone(store);
        let removed = tokio::task::spawn_blocking(move || store.sweep_expired(now, batch))
            .await
            .map_err(|_| LoftError::NotReady)??;
        if removed == 0 {
            break;
        }
        total = total.saturating_add(removed);
        tokio::task::yield_now().await;
    }
    if total > 0 {
        let store = Arc::clone(store);
        tokio::task::spawn_blocking(move || store.retention_checkpoint())
            .await
            .map_err(|_| LoftError::NotReady)??;
        tracing::info!(removed = total, "retention sweep complete");
    }
    Ok(total)
}

/// Compatibility runner for callers that own process lifetime. A failure terminates the future so
/// its task supervisor can mark readiness false or stop the process.
pub async fn run(store: Arc<dyn LoftStore>, interval_secs: u64, batch: usize) -> Result<()> {
    let (_stop_tx, stop_rx) = watch::channel(false);
    run_until(store, interval_secs, batch, stop_rx).await
}

pub(crate) async fn run_until(
    store: Arc<dyn LoftStore>,
    interval_secs: u64,
    batch: usize,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    if interval_secs == 0 {
        return Err(LoftError::Configuration(
            "retention interval must be nonzero",
        ));
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                sweep_once(&store, batch).await?;
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, StorageStats, StoredEvent};
    use pigeonpost_core::{envelope, AgentRecord, Identity, RecipientPolicy, Wrap};

    struct FailingStore;

    impl LoftStore for FailingStore {
        fn admit(
            &self,
            _: &Wrap,
            _: &[u8; 32],
            _: u64,
            _: u64,
            _: u64,
            _: Option<u64>,
        ) -> Result<bool> {
            Err(LoftError::NotReady)
        }

        fn fetch(&self, _: &[u8; 32], _: u64, _: usize) -> Result<Vec<StoredEvent>> {
            Err(LoftError::NotReady)
        }

        fn policy(&self, _: &[u8; 32]) -> Result<Option<RecipientPolicy>> {
            Err(LoftError::NotReady)
        }

        fn put_policy(&self, _: &RecipientPolicy, _: u64) -> Result<()> {
            Err(LoftError::NotReady)
        }

        fn agent_record(&self, _: &str) -> Result<Option<AgentRecord>> {
            Err(LoftError::NotReady)
        }

        fn put_agent_record(&self, _: &str, _: &AgentRecord, _: u64) -> Result<()> {
            Err(LoftError::NotReady)
        }

        fn sweep_expired(&self, _: u64, _: usize) -> Result<usize> {
            Err(LoftError::NotReady)
        }

        fn retention_checkpoint(&self) -> Result<()> {
            Err(LoftError::NotReady)
        }

        fn stats(&self) -> Result<StorageStats> {
            Err(LoftError::NotReady)
        }

        fn health_check(&self) -> Result<()> {
            Err(LoftError::NotReady)
        }
    }

    #[tokio::test]
    async fn sweeps_everything_expired_across_several_batches() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);
        for index in 0..25 {
            let wrap =
                envelope::wrap(&alice, &bob.verifying_key(), &format!("m{index}"), 1_000).unwrap();
            store
                .admit(&wrap, &wrap.id(), 0, 1, u64::MAX, None)
                .unwrap();
        }
        assert_eq!(sweep_once(&store, 4).await.unwrap(), 25);
        assert_eq!(store.event_count().unwrap(), 0);
    }

    #[tokio::test]
    async fn zero_batch_or_interval_is_a_supervised_failure() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        assert!(sweep_once(&store, 0).await.is_err());
        assert!(run(store, 0, 1).await.is_err());
    }

    #[tokio::test]
    async fn database_failure_terminates_the_retention_future() {
        let store: Arc<dyn LoftStore> = Arc::new(FailingStore);
        assert!(matches!(run(store, 1, 1).await, Err(LoftError::NotReady)));
    }

    #[tokio::test]
    async fn completed_retention_pass_truncates_the_wal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private-storage/loft.db");
        let store: Arc<dyn LoftStore> =
            Arc::new(SqliteStore::open(path.to_str().unwrap()).unwrap());
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);
        let wrap = envelope::wrap(&alice, &bob.verifying_key(), "expired", 1_000).unwrap();
        store
            .admit(&wrap, &wrap.id(), 0, 1, u64::MAX, None)
            .unwrap();
        assert_eq!(sweep_once(&store, 10).await.unwrap(), 1);

        let wal_path = std::path::PathBuf::from(format!("{}-wal", path.display()));
        if let Ok(metadata) = std::fs::metadata(wal_path) {
            assert_eq!(
                metadata.len(),
                0,
                "retained ciphertext must leave no WAL frames"
            );
        }
    }
}
