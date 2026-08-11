//! # Pigeonpost loft
//!
//! A loft is a durable inbox: it holds gift-wrapped mail addressed to a public key until the
//! recipient next wakes up, which may be weeks. It is deliberately dumb storage — no per-client
//! state, no routing table, no forwarding — because that is what keeps a useful loft cheap enough
//! for a stranger to donate (`docs/capacity.md`).
//!
//! What a loft can see: a blob, and the pubkey it is addressed to. Not the sender, not the real
//! send time, not the content, not even how long the message is beyond a 256-byte bucket.
//!
//! ## Both halves live here
//!
//! [`Loft`] is the server; [`LoftClient`] is how a client talks to one. Keeping them in one crate
//! means the protocol has exactly one definition, and the integration tests exercise the real
//! wire format rather than a mock.
//!
//! ```no_run
//! # #[cfg(feature = "server")]
//! # mod server_example {
//! # use std::sync::Arc;
//! # use pigeonpost_loft::{Loft, LoftConfig, SqliteStore};
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Arc::new(SqliteStore::open("mail.db")?);
//! let loft = Arc::new(Loft::new(
//!     LoftConfig::new([0u8; 32], "http://127.0.0.1:7717"),
//!     store,
//! )?);
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:7717").await?;
//! pigeonpost_loft::serve(listener, loft, async {
//!     let _ = tokio::signal::ctrl_c().await;
//! }).await?;
//! # Ok(())
//! # }
//! # }
//! ```

#![forbid(unsafe_code)]

#[cfg(feature = "server")]
pub mod attribution;
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod error;
#[cfg(feature = "server")]
pub mod limiter;
#[cfg(feature = "server")]
pub mod registry_keys;
#[cfg(feature = "server")]
pub mod retention;
#[cfg(feature = "server")]
pub mod sealed_trace;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod store;
#[cfg(feature = "server")]
pub mod trace;
pub mod wire;

pub use wire::{MAX_EVENT_BYTES, MAX_RETENTION_DAYS};

#[cfg(feature = "server")]
pub use attribution::{
    AttributionKeyResolver, AttributionResolutionError, ResolvedAttributionKey,
    UnconfiguredAttributionResolver,
};
#[cfg(feature = "client")]
pub use client::{ClientError, LoftClient, LoftEndpoint, RefusalCode};
#[cfg(feature = "server")]
pub use config::{
    LoftConfig, MAX_BLOCKING_OPERATIONS, MAX_CAPACITY_BYTES, MAX_CONCURRENT_CONNECTIONS,
    MAX_CONCURRENT_REQUESTS, MAX_FETCH_LIMIT, MAX_FETCH_RESPONSE_BYTES,
    MAX_GLOBAL_REQUESTS_PER_MINUTE, MAX_HEADER_TIMEOUT_SECS, MAX_LIMITER_KEYS,
    MAX_RATE_BYTES_PER_MINUTE, MAX_RATE_LIMIT_PER_MINUTE, MAX_REQUEST_BYTES,
    MAX_REQUEST_TIMEOUT_SECS, MAX_RESPONSE_TIMEOUT_SECS, MAX_SWEEP_BATCH, MAX_SWEEP_INTERVAL_SECS,
    MAX_TRACE_TIMEOUT_MS, MAX_TRUSTED_PROXIES,
};
#[cfg(feature = "server")]
pub use error::{LoftError, Result};
#[cfg(feature = "server")]
pub use registry_keys::{
    CheckpointPin, WitnessKeyConfig, WitnessedRegistryConfig, WitnessedRegistryKeyCache,
};
#[cfg(feature = "server")]
pub use sealed_trace::{
    CapturePolicy, ResolvedTraceKey, SealedTraceConfig, SealedTraceSink, TraceKeyResolver,
};
#[cfg(feature = "server")]
pub use server::Loft;
#[cfg(feature = "server")]
pub use store::{
    LoftStore, SqliteStore, StorageStats, StoredEvent, TraceSegmentCatalog, TraceSegmentMetadata,
    TraceSegmentState,
};
#[cfg(feature = "server")]
pub use trace::{
    NoopTraceSink, TraceCapacity, TraceInput, TraceOperation, TraceSink, TraceSinkError,
};

/// Serve a loft until `shutdown` resolves.
///
/// Wrapping `axum::serve` here keeps the axum version an implementation detail of this crate —
/// callers (the CLI, tests) do not need to depend on it directly.
#[cfg(feature = "server")]
pub async fn serve<F>(
    listener: tokio::net::TcpListener,
    loft: std::sync::Arc<Loft>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    use std::io;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let listener_address = listener.local_addr()?;
    if !is_private_loopback_listener(listener_address, &loft.config.origin) {
        if !loft.trace_sink.enabled() || !loft.attribution_resolver.configured() {
            return Err(io::Error::other(
                "public loft compliance configuration is incomplete",
            ));
        }
        if loft.trace_sink.as_ref().type_id() != std::any::TypeId::of::<SealedTraceSink>()
            || loft.store.as_ref().type_id() != std::any::TypeId::of::<SqliteStore>()
            || !loft.store.supports_public_durable_trace_admission()
        {
            return Err(io::Error::other(
                "public loft requires the audited sealed-trace and durable SQLite adapters",
            ));
        }
        if !trace_capacity_covers(
            loft.trace_sink.as_ref(),
            loft.config.global_requests_per_minute,
        ) {
            return Err(io::Error::other(
                "public loft trace capacity contract is insufficient",
            ));
        }
    }

    if loft.attribution_resolver.configured()
        && loft.attribution_resolver.refresh_interval_ms().is_some()
    {
        loft.attribution_resolver
            .refresh()
            .await
            .map_err(|_| io::Error::other("attribution registry refresh failed"))?;
    }

    loft.validate_startup()
        .map_err(|_| io::Error::other("loft startup validation failed"))?;
    loft.set_ready(true);

    let (retention_stop_tx, retention_stop_rx) = tokio::sync::watch::channel(false);
    let retention_store = std::sync::Arc::clone(&loft.store);
    let interval = loft.config.sweep_interval_secs;
    let batch = loft.config.sweep_batch;
    let mut retention_task = tokio::spawn(async move {
        retention::run_until(retention_store, interval, batch, retention_stop_rx).await
    });

    let (attribution_stop_tx, mut attribution_stop_rx) = tokio::sync::watch::channel(false);
    let attribution_resolver = std::sync::Arc::clone(&loft.attribution_resolver);
    let trace_sink = std::sync::Arc::clone(&loft.trace_sink);
    let refresh_interval = attribution_resolver.refresh_interval_ms();
    let mut attribution_task = tokio::spawn(async move {
        loop {
            let Some(interval_ms) = refresh_interval else {
                attribution_stop_rx
                    .changed()
                    .await
                    .map_err(|_| AttributionResolutionError::Unavailable)?;
                return Ok::<(), AttributionResolutionError>(());
            };
            tokio::select! {
                changed = attribution_stop_rx.changed() => {
                    changed.map_err(|_| AttributionResolutionError::Unavailable)?;
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {
                    let _ = attribution_resolver.refresh().await;
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
                        .unwrap_or(0);
                    attribution_resolver.readiness(now_ms)?;
                    trace_sink
                        .readiness()
                        .map_err(|_| AttributionResolutionError::Unavailable)?;
                }
            }
        }
    });

    let (server_stop_tx, server_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let router = std::sync::Arc::clone(&loft).router();
    let http_config = loft.config.clone();
    let mut server_task = tokio::spawn(async move {
        server::serve_http(listener, router, &http_config, server_stop_rx).await
    });

    tokio::pin!(shutdown);
    enum Stop {
        Requested,
        Retention(std::result::Result<std::result::Result<(), LoftError>, tokio::task::JoinError>),
        Attribution(
            std::result::Result<
                std::result::Result<(), AttributionResolutionError>,
                tokio::task::JoinError,
            >,
        ),
        Server(std::result::Result<std::io::Result<()>, tokio::task::JoinError>),
    }
    let stop = tokio::select! {
        _ = &mut shutdown => Stop::Requested,
        result = &mut retention_task => Stop::Retention(result),
        result = &mut attribution_task => Stop::Attribution(result),
        result = &mut server_task => Stop::Server(result),
    };

    loft.set_ready(false);
    let _ = retention_stop_tx.send(true);
    let _ = attribution_stop_tx.send(true);
    let _ = server_stop_tx.send(());

    let close_trace = || async {
        let sink = std::sync::Arc::clone(&loft.trace_sink);
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        tokio::time::timeout(
            Duration::from_secs(loft.config.request_timeout_secs),
            tokio::task::spawn_blocking(move || sink.shutdown(timestamp_ms)),
        )
        .await
        .map_err(|_| io::Error::other("trace shutdown timed out"))?
        .map_err(|_| io::Error::other("trace shutdown task failed"))?
        .map_err(|_| io::Error::other("trace shutdown failed"))
    };

    let grace = Duration::from_secs(
        loft.config
            .request_timeout_secs
            .saturating_add(1)
            .clamp(1, 60),
    );
    let result = match stop {
        Stop::Requested => {
            let deadline = tokio::time::Instant::now() + grace;
            let server = drain_task_until(deadline, &mut server_task).await;
            let retention = drain_task_until(deadline, &mut retention_task).await;
            let attribution = drain_task_until(deadline, &mut attribution_task).await;
            match (server, retention, attribution) {
                (Some(server), Some(retention), Some(attribution)) => server_outcome(server)
                    .and_then(|()| retention_outcome(retention))
                    .and_then(|()| attribution_outcome(attribution)),
                _ => Err(io::Error::other("loft graceful drain timed out")),
            }
        }
        Stop::Retention(result) => {
            let deadline = tokio::time::Instant::now() + grace;
            let server = drain_task_until(deadline, &mut server_task).await;
            let attribution = drain_task_until(deadline, &mut attribution_task).await;
            if server.is_none() || attribution.is_none() {
                Err(io::Error::other("loft graceful drain timed out"))
            } else {
                match result {
                    Ok(Ok(())) => Err(io::Error::other("retention supervisor stopped")),
                    Ok(Err(_)) | Err(_) => Err(io::Error::other("retention sweep failed")),
                }
            }
        }
        Stop::Attribution(result) => {
            let deadline = tokio::time::Instant::now() + grace;
            let server = drain_task_until(deadline, &mut server_task).await;
            let retention = drain_task_until(deadline, &mut retention_task).await;
            if server.is_none() || retention.is_none() {
                Err(io::Error::other("loft graceful drain timed out"))
            } else {
                match result {
                    Ok(Ok(())) => Err(io::Error::other("attribution refresh supervisor stopped")),
                    Ok(Err(_)) | Err(_) => {
                        Err(io::Error::other("attribution registry refresh failed"))
                    }
                }
            }
        }
        Stop::Server(result) => {
            let deadline = tokio::time::Instant::now() + grace;
            let retention = drain_task_until(deadline, &mut retention_task).await;
            let attribution = drain_task_until(deadline, &mut attribution_task).await;
            if retention.is_none() || attribution.is_none() {
                Err(io::Error::other("loft graceful drain timed out"))
            } else {
                match server_outcome(result) {
                    Ok(()) => Err(io::Error::other("loft server stopped unexpectedly")),
                    Err(error) => Err(error),
                }
            }
        }
    };
    let trace_result = close_trace().await;
    trace_result.and(result)
}

#[cfg(feature = "server")]
fn trace_capacity_covers(sink: &dyn TraceSink, required_records_per_minute: u32) -> bool {
    sink.capacity_contract().is_some_and(|contract| {
        contract
            .policy
            .required_capacity_epochs()
            .is_ok_and(|required_epochs| contract.utc_epochs >= required_epochs)
            && contract.records_per_minute >= required_records_per_minute
            && contract.max_records_per_segment > 0
            && pigeonpost_compliance_seal::required_network_trace_storage_bytes(
                contract.records_per_minute,
                contract.utc_epochs,
                contract.max_records_per_segment,
            )
            .is_ok_and(|required| required <= contract.logical_limit_bytes)
    })
}

#[cfg(feature = "server")]
fn is_private_loopback_listener(address: std::net::SocketAddr, origin: &str) -> bool {
    if !address.ip().is_loopback() {
        return false;
    }

    let Ok(origin) = reqwest::Url::parse(origin) else {
        return false;
    };
    let Some(host) = origin.host_str() else {
        return false;
    };
    let Ok(origin_ip) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() else {
        return false;
    };

    origin.scheme() == "http"
        && origin_ip == address.ip()
        && origin.port_or_known_default() == Some(address.port())
}

#[cfg(feature = "server")]
async fn drain_task_until<T>(
    deadline: tokio::time::Instant,
    task: &mut tokio::task::JoinHandle<T>,
) -> Option<std::result::Result<T, tokio::task::JoinError>> {
    match tokio::time::timeout_at(deadline, &mut *task).await {
        Ok(result) => Some(result),
        Err(_) => {
            task.abort();
            let _ = task.await;
            None
        }
    }
}

#[cfg(feature = "server")]
fn server_outcome(
    result: std::result::Result<std::io::Result<()>, tokio::task::JoinError>,
) -> std::io::Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(std::io::Error::other("loft server task failed")),
    }
}

#[cfg(feature = "server")]
fn retention_outcome(
    result: std::result::Result<std::result::Result<(), LoftError>, tokio::task::JoinError>,
) -> std::io::Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(std::io::Error::other("retention sweep failed")),
    }
}

#[cfg(feature = "server")]
fn attribution_outcome(
    result: std::result::Result<
        std::result::Result<(), AttributionResolutionError>,
        tokio::task::JoinError,
    >,
) -> std::io::Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(std::io::Error::other("attribution registry refresh failed")),
    }
}

#[cfg(all(test, feature = "server"))]
mod serve_tests {
    use super::*;
    use pigeonpost_compliance_format::{
        ComplianceKeyId, Jurisdiction, TraceCapturePolicy, TraceRetentionPolicy,
    };
    use pigeonpost_core::{AgentRecord, RecipientPolicy, Wrap};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SweepFailureStore {
        inner: SqliteStore,
    }

    struct RecordingTraceSink {
        shutdowns: std::sync::Arc<AtomicUsize>,
    }

    struct PlannedTraceSink(TraceCapacity);

    struct ConfiguredAttributionResolver;

    impl AttributionKeyResolver for ConfiguredAttributionResolver {
        fn resolve(
            &self,
            _key_id: &ComplianceKeyId,
        ) -> std::result::Result<Option<ResolvedAttributionKey>, AttributionResolutionError>
        {
            Ok(None)
        }
    }

    fn us_trace_policy() -> TraceRetentionPolicy {
        TraceRetentionPolicy {
            jurisdiction: Jurisdiction::Us,
            capture: TraceCapturePolicy::Standing,
            retention_days: Some(30),
        }
    }

    impl TraceSink for PlannedTraceSink {
        fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
            Ok(())
        }

        fn capacity_contract(&self) -> Option<TraceCapacity> {
            Some(self.0)
        }

        fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
            Ok(())
        }
    }

    impl TraceSink for RecordingTraceSink {
        fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
            Ok(())
        }

        fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
            Ok(())
        }

        fn shutdown(&self, _timestamp_ms: u64) -> std::result::Result<(), TraceSinkError> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl LoftStore for SweepFailureStore {
        fn admit(
            &self,
            wrap: &Wrap,
            id: &[u8; 32],
            stored_at: u64,
            expires_at: u64,
            capacity_bytes: u64,
            expected_policy_seq: Option<u64>,
        ) -> Result<bool> {
            self.inner.admit(
                wrap,
                id,
                stored_at,
                expires_at,
                capacity_bytes,
                expected_policy_seq,
            )
        }

        fn fetch(
            &self,
            recipient: &[u8; 32],
            cursor: u64,
            limit: usize,
        ) -> Result<Vec<StoredEvent>> {
            self.inner.fetch(recipient, cursor, limit)
        }

        fn policy(&self, pubkey: &[u8; 32]) -> Result<Option<RecipientPolicy>> {
            self.inner.policy(pubkey)
        }

        fn put_policy(&self, policy: &RecipientPolicy, capacity_bytes: u64) -> Result<()> {
            self.inner.put_policy(policy, capacity_bytes)
        }

        fn agent_record(&self, address: &str) -> Result<Option<AgentRecord>> {
            self.inner.agent_record(address)
        }

        fn put_agent_record(
            &self,
            address: &str,
            record: &AgentRecord,
            capacity_bytes: u64,
        ) -> Result<()> {
            self.inner.put_agent_record(address, record, capacity_bytes)
        }

        fn sweep_expired(&self, _now: u64, _batch: usize) -> Result<usize> {
            Err(LoftError::NotReady)
        }

        fn retention_checkpoint(&self) -> Result<()> {
            self.inner.retention_checkpoint()
        }

        fn stats(&self) -> Result<StorageStats> {
            self.inner.stats()
        }

        fn health_check(&self) -> Result<()> {
            self.inner.health_check()
        }

        fn supports_durable_trace_admission(&self) -> bool {
            self.inner.supports_durable_trace_admission()
        }

        fn charge_trace_admission(&self, timestamp_ms: u64, limit: u32) -> Result<()> {
            self.inner.charge_trace_admission(timestamp_ms, limit)
        }
    }

    #[tokio::test]
    async fn coordinated_shutdown_waits_for_server_and_retention() {
        let store: std::sync::Arc<dyn LoftStore> =
            std::sync::Arc::new(SqliteStore::in_memory().unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let loft = std::sync::Arc::new(Loft::new(LoftConfig::new([1; 32], origin), store).unwrap());
        serve(listener, loft, async {}).await.unwrap();
    }

    #[tokio::test]
    async fn public_listener_rejects_development_dependencies() {
        let store: std::sync::Arc<dyn LoftStore> =
            std::sync::Arc::new(SqliteStore::in_memory().unwrap());
        let loft = std::sync::Arc::new(
            Loft::new(LoftConfig::new([1; 32], "https://loft.example"), store).unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();

        let error = serve(listener, loft, async {}).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            error.to_string(),
            "public loft compliance configuration is incomplete"
        );
    }

    #[tokio::test]
    async fn public_listener_rejects_self_asserted_trace_adapters() {
        let storage = tempfile::tempdir().unwrap();
        let database = storage.path().join("private-storage/loft.db");
        let store: std::sync::Arc<dyn LoftStore> =
            std::sync::Arc::new(SqliteStore::open(database.to_str().unwrap()).unwrap());
        let trace: std::sync::Arc<dyn TraceSink> =
            std::sync::Arc::new(PlannedTraceSink(TraceCapacity {
                policy: us_trace_policy(),
                records_per_minute: 2_400,
                utc_epochs: 31,
                max_records_per_segment: 1_000,
                logical_limit_bytes: 64 * 1024 * 1024 * 1024,
            }));
        let loft = std::sync::Arc::new(
            Loft::new(LoftConfig::new([1; 32], "https://loft.example"), store)
                .unwrap()
                .with_attribution_resolver(std::sync::Arc::new(ConfiguredAttributionResolver))
                .with_trace_sink(trace),
        );
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();

        let error = serve(listener, loft, async {}).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "public loft requires the audited sealed-trace and durable SQLite adapters"
        );
    }

    #[test]
    fn public_capacity_contract_must_cover_the_actual_global_admission_rate() {
        let contract = |records_per_minute| {
            PlannedTraceSink(TraceCapacity {
                policy: us_trace_policy(),
                records_per_minute,
                utc_epochs: 31,
                max_records_per_segment: 1_000,
                logical_limit_bytes: 64 * 1024 * 1024 * 1024,
            })
        };
        assert!(!trace_capacity_covers(&contract(2_399), 2_400));
        assert!(trace_capacity_covers(&contract(2_400), 2_400));
        let understated = PlannedTraceSink(TraceCapacity {
            policy: us_trace_policy(),
            records_per_minute: 2_400,
            utc_epochs: 31,
            max_records_per_segment: 1_000,
            logical_limit_bytes: 1,
        });
        assert!(!trace_capacity_covers(&understated, 2_400));
        let short_runway = PlannedTraceSink(TraceCapacity {
            policy: us_trace_policy(),
            records_per_minute: 2_400,
            utc_epochs: 30,
            max_records_per_segment: 1_000,
            logical_limit_bytes: 64 * 1024 * 1024 * 1024,
        });
        assert!(!trace_capacity_covers(&short_runway, 2_400));
        let invalid_policy = PlannedTraceSink(TraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Us,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(29),
            },
            records_per_minute: 2_400,
            utc_epochs: 31,
            max_records_per_segment: 1_000,
            logical_limit_bytes: 64 * 1024 * 1024 * 1024,
        });
        assert!(!trace_capacity_covers(&invalid_policy, 2_400));
    }

    #[tokio::test]
    async fn retention_failure_stops_the_server() {
        let store: std::sync::Arc<dyn LoftStore> = std::sync::Arc::new(SweepFailureStore {
            inner: SqliteStore::in_memory().unwrap(),
        });
        let shutdowns = std::sync::Arc::new(AtomicUsize::new(0));
        let trace: std::sync::Arc<dyn TraceSink> = std::sync::Arc::new(RecordingTraceSink {
            shutdowns: std::sync::Arc::clone(&shutdowns),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let loft = std::sync::Arc::new(
            Loft::new(LoftConfig::new([1; 32], origin), store)
                .unwrap()
                .with_trace_sink(trace),
        );
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            serve(listener, loft, std::future::pending()),
        )
        .await
        .expect("the failed retention task must stop the server");
        assert!(result.is_err());
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }
}
