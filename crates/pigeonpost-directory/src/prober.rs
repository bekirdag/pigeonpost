//! The prober.
//!
//! Treats every loft as untrusted and measures rather than asks. The network boundary here is
//! deliberately stricter than a general HTTP client: public HTTPS only, DNS pinned for the whole
//! probe, no redirects or proxies, and bounded time, bodies, pagination, and concurrency.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pigeonpost_core::{
    envelope,
    network::{is_localhost_name, is_public_network_address as is_public_ip},
    FetchAuth, Identity,
};
use pigeonpost_loft::wire::{
    FetchRequest, FetchResponse, InfoResponse, PublishRequest, PublishResponse,
};
use reqwest::{Client, RequestBuilder, Url};
use serde::de::DeserializeOwned;
use tokio::task::JoinSet;
use zeroize::Zeroize;

use crate::directory::{
    Directory, ProbeResult, RetentionUpdate, RetentionWork, MAX_PROBE_CANDIDATES,
};
use crate::entry::{parse_hex32, DirectoryEntry};
use crate::error::{DirectoryError, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const INFO_BODY_LIMIT: usize = 64 * 1024;
const MUTATION_BODY_LIMIT: usize = 64 * 1024;
const FETCH_BODY_LIMIT: usize = 4 * 1024 * 1024;
const FETCH_PAGE_SIZE: usize = 500;
const GIB_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FETCH_PAGES: usize = 8;
const MAX_RESOLVED_ADDRESSES: usize = 16;
const MAX_CONCURRENT_PROBES: usize = 16;
// Match the network fan-out so an ordinary sweep never penalizes a loft for local scheduler
// pressure. Programmatic callers above that audited fan-out fail immediately instead of building a
// second queue behind Tokio's blocking pool.
const MAX_PROBER_CPU_OPERATIONS: usize = MAX_CONCURRENT_PROBES;
const SWEEP_DEADLINE: Duration = Duration::from_secs(45);
const MAX_BLOCKING_DIRECTORY_OPERATIONS: usize = 1;
const BLOCKING_DIRECTORY_TIMEOUT: Duration = Duration::from_secs(6);
const PROBER_CPU_BUSY: &str = "local probe response processing is saturated";
const PROBER_CPU_FAILED: &str = "local probe response processor failed";
const MAX_PROBE_EVENT_AGE_SECS: u64 =
    pigeonpost_core::envelope::MAX_TIMESTAMP_JITTER_SECS + 15 * 60;

#[derive(Clone, Copy)]
struct NetworkPolicy {
    allow_http: bool,
    allow_non_public_addresses: bool,
}

const PRODUCTION_NETWORK_POLICY: NetworkPolicy = NetworkPolicy {
    allow_http: false,
    allow_non_public_addresses: false,
};

struct ProbeOutcome {
    result: ProbeResult,
    retention_update: Option<RetentionUpdate>,
}

struct ProcessedFetchPage {
    found: Option<pigeonpost_core::envelope::Wrap>,
    next_cursor: u64,
    more: bool,
}

fn blocking_directory_limiter() -> Arc<tokio::sync::Semaphore> {
    static LIMITER: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_BLOCKING_DIRECTORY_OPERATIONS,
        ))
    }))
}

async fn blocking_directory<T, F>(task: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    blocking_directory_with(
        blocking_directory_limiter(),
        BLOCKING_DIRECTORY_TIMEOUT,
        task,
    )
    .await
}

async fn blocking_directory_with<T, F>(
    limiter: Arc<tokio::sync::Semaphore>,
    timeout: Duration,
    task: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let permit = tokio::time::timeout_at(deadline, limiter.acquire_owned())
        .await
        .map_err(|_| DirectoryError::Overloaded)?
        .map_err(|_| DirectoryError::Overloaded)?;
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        task()
    });
    tokio::time::timeout_at(deadline, worker)
        .await
        .map_err(|_| DirectoryError::Overloaded)?
        .map_err(|_| DirectoryError::Io(std::io::Error::other("prober worker failed")))?
}

fn prober_cpu_limiter() -> Arc<tokio::sync::Semaphore> {
    static LIMITER: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(
        LIMITER.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_PROBER_CPU_OPERATIONS))),
    )
}

async fn prober_cpu<T, F>(task: F) -> std::result::Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> std::result::Result<T, String> + Send + 'static,
{
    prober_cpu_with(prober_cpu_limiter(), task).await
}

async fn prober_cpu_with<T, F>(
    limiter: Arc<tokio::sync::Semaphore>,
    task: F,
) -> std::result::Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> std::result::Result<T, String> + Send + 'static,
{
    // The permit moves into the non-cancelable closure. If a sweep deadline drops this future, the
    // blocking work remains counted until it really exits instead of opening an unbounded detached
    // tail. Admission itself never waits behind work already accepted by the blocking pool.
    let permit = limiter
        .try_acquire_owned()
        .map_err(|_| PROBER_CPU_BUSY.to_owned())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        task()
    })
    .await
    .map_err(|_| PROBER_CPU_FAILED.to_owned())?
}

#[cfg(feature = "test-utilities")]
const TEST_NETWORK_POLICY: NetworkPolicy = NetworkPolicy {
    allow_http: true,
    allow_non_public_addresses: true,
};

/// Probe one submitted loft through the production network policy.
///
/// The complete signed entry is the claim being checked. The probe stops before writing when
/// `/v1/info` presents another key, origin, capacity, retention period, protocol, or policy.
pub async fn probe_once(identity: &Identity, entry: &DirectoryEntry, now: u64) -> ProbeResult {
    probe_once_with_policy(identity, entry, now, PRODUCTION_NETWORK_POLICY).await
}

/// Explicit local-network allowance for this crate's in-process acceptance tests.
///
/// The symbol does not exist unless the opt-in `test-utilities` feature is enabled.
#[cfg(feature = "test-utilities")]
#[doc(hidden)]
pub async fn probe_once_for_test(
    identity: &Identity,
    entry: &DirectoryEntry,
    now: u64,
) -> ProbeResult {
    probe_once_with_policy(identity, entry, now, TEST_NETWORK_POLICY).await
}

async fn probe_once_with_policy(
    identity: &Identity,
    entry: &DirectoryEntry,
    now: u64,
    policy: NetworkPolicy,
) -> ProbeResult {
    probe_once_with_retention(identity, entry, now, policy, None)
        .await
        .result
}

async fn probe_once_with_retention(
    identity: &Identity,
    entry: &DirectoryEntry,
    now: u64,
    policy: NetworkPolicy,
    retention_work: Option<RetentionWork>,
) -> ProbeOutcome {
    let endpoint = entry.endpoint.as_str();
    let wall_clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(now);
    let expected_key = match entry.verify() {
        Ok(key) => key.to_bytes(),
        Err(_) => {
            return failure(
                endpoint,
                now,
                false,
                1.0,
                "signed directory submission is invalid",
            )
        }
    };
    let (client, base_url) = match pinned_client(endpoint, policy).await {
        Ok(prepared) => prepared,
        Err(detail) => return failure(endpoint, now, false, 1.0, detail),
    };

    let info: InfoResponse =
        match get_json(&client, endpoint_url(&base_url, "v1/info"), INFO_BODY_LIMIT).await {
            Ok(info) => info,
            Err(detail) => return failure(endpoint, now, false, 1.0, detail),
        };
    let expected_origin = base_url.as_str().trim_end_matches('/');
    if let Err(detail) = validate_info_claim(&info, entry, expected_origin, &expected_key) {
        // The live counters are untrusted until the whole claim validates. Persist the worst safe
        // weight instead of letting a finite-but-out-of-range value poison the measurement sweep.
        return failure(endpoint, now, true, 1.0, detail);
    }

    // A fresh recipient prevents the prober's own retained history from growing without bound.
    // The persistent prober identity remains the hidden sender; only the one-shot fetch owner
    // rotates. Pagination below remains a bounded defense against a dishonest response.
    let probe_recipient = Identity::generate();
    let body = format!("pigeonpost prober {now}");
    let wrap = match envelope::wrap(identity, &probe_recipient.verifying_key(), &body, now) {
        Ok(wrap) => wrap,
        Err(_) => {
            return failure(
                endpoint,
                now,
                true,
                info.utilization,
                "could not construct probe event",
            )
        }
    };
    let id = wrap.id();

    let publish: PublishResponse = match post_json(
        &client,
        endpoint_url(&base_url, "v1/publish"),
        &PublishRequest { wrap, token: None },
        MUTATION_BODY_LIMIT,
    )
    .await
    {
        Ok(response) => response,
        Err(detail) => return failure(endpoint, now, true, info.utilization, detail),
    };
    if publish.id != crate::entry::hex(&id) {
        return failure(
            endpoint,
            now,
            true,
            info.utilization,
            "loft acknowledged a different probe event",
        );
    }

    let found = match fetch_event(
        &client,
        &base_url,
        &probe_recipient,
        &expected_key,
        wall_clock,
        id,
    )
    .await
    {
        Ok(found) => found,
        Err(detail) => return failure(endpoint, now, true, info.utilization, detail),
    };
    if found.created_at > now || now.saturating_sub(found.created_at) > MAX_PROBE_EVENT_AGE_SECS {
        return failure(
            endpoint,
            now,
            true,
            info.utilization,
            "loft returned the probe event outside the accepted age window",
        );
    }

    let result = ProbeResult {
        endpoint: endpoint.to_string(),
        at: now,
        reachable: true,
        stored_and_returned: true,
        utilization: info.utilization,
        retention_age_secs: None,
        retention_ok: None,
        detail: None,
    };
    apply_retention_work(
        identity,
        &client,
        &base_url,
        &expected_key,
        wall_clock,
        now,
        result,
        retention_work,
    )
    .await
}

fn validate_info_claim(
    info: &InfoResponse,
    entry: &DirectoryEntry,
    expected_origin: &str,
    expected_key: &[u8; 32],
) -> std::result::Result<(), &'static str> {
    if info.protocol != pigeonpost_core::PROTOCOL_VERSION {
        return Err("loft protocol does not match this release");
    }
    if info.origin != expected_origin
        || pigeonpost_core::fetch_auth::validate_loft_origin(&info.origin).is_err()
    {
        return Err("loft info is bound to a different origin");
    }
    if parse_hex32(&info.pubkey).as_ref() != Some(expected_key) {
        return Err("loft key does not match the signed directory submission");
    }
    let expected_capacity = entry
        .capacity_gb
        .checked_mul(GIB_BYTES)
        .ok_or("signed loft capacity is outside the supported range")?;
    if info.capacity_bytes != expected_capacity {
        return Err("live loft capacity does not match the signed directory submission");
    }
    if info.retention_days != entry.retention_days {
        return Err("live loft retention does not match the signed directory submission");
    }
    if info.open != entry.policy.open
        || info.pow_floor != entry.policy.pow_floor
        || info.max_event_bytes != entry.policy.max_event_bytes
    {
        return Err("live loft policy does not match the signed directory submission");
    }
    if info.capacity_bytes == 0
        || info.used_bytes > info.capacity_bytes
        || !info.utilization.is_finite()
    {
        return Err("live loft utilization is invalid");
    }
    let computed = (info.used_bytes as f64 / info.capacity_bytes as f64).min(1.0);
    if (info.utilization - computed).abs() > f64::EPSILON * 4.0 {
        return Err("live loft utilization does not match its authenticated counters");
    }
    Ok(())
}

async fn fetch_event(
    client: &Client,
    base_url: &Url,
    recipient: &Identity,
    expected_key: &[u8; 32],
    wall_clock: u64,
    event_id: [u8; 32],
) -> std::result::Result<pigeonpost_core::envelope::Wrap, String> {
    // Fetch pages with a strict progress and work budget. Each canary has a one-shot recipient,
    // but bounded pagination also contains a dishonest loft that fabricates unrelated events.
    let mut cursor = 0;
    for _ in 0..MAX_FETCH_PAGES {
        let auth = FetchAuth::new(
            recipient,
            expected_key,
            base_url.as_str().trim_end_matches('/'),
            wall_clock / 60,
            cursor,
        )
        .map_err(|_| "loft origin cannot bind fetch authentication".to_string())?;
        let request = FetchRequest {
            auth,
            limit: Some(FETCH_PAGE_SIZE),
        };
        let body = receive_body(
            client
                .post(endpoint_url(base_url, "v1/fetch"))
                .json(&request),
            FETCH_BODY_LIMIT,
        )
        .await?;
        let ProcessedFetchPage {
            found,
            next_cursor,
            more,
        } = decode_fetch_page(body, event_id).await?;
        if let Some(found) = found {
            return Ok(found);
        }
        if !more {
            return Err("loft did not return the expected probe event".into());
        }
        if next_cursor <= cursor {
            return Err("loft returned a non-advancing fetch cursor".into());
        }
        cursor = next_cursor;
    }
    Err("probe fetch exceeded the bounded pagination budget".into())
}

async fn decode_fetch_page(
    body: Vec<u8>,
    event_id: [u8; 32],
) -> std::result::Result<ProcessedFetchPage, String> {
    decode_fetch_page_with(prober_cpu_limiter(), body, event_id).await
}

async fn decode_fetch_page_with(
    limiter: Arc<tokio::sync::Semaphore>,
    body: Vec<u8>,
    event_id: [u8; 32],
) -> std::result::Result<ProcessedFetchPage, String> {
    prober_cpu_with(limiter, move || process_fetch_page(&body, event_id)).await
}

fn process_fetch_page(
    body: &[u8],
    event_id: [u8; 32],
) -> std::result::Result<ProcessedFetchPage, String> {
    let response: FetchResponse =
        serde_json::from_slice(body).map_err(|_| "loft returned malformed JSON".to_owned())?;
    let FetchResponse {
        events,
        next_cursor,
        more,
    } = response;
    let found = events.into_iter().find(|event| event.id() == event_id);
    Ok(ProcessedFetchPage {
        found,
        next_cursor,
        more,
    })
}

#[cfg(test)]
async fn decode_fetch_page_with_observer<F>(
    limiter: Arc<tokio::sync::Semaphore>,
    body: Vec<u8>,
    event_id: [u8; 32],
    observe_decode_thread: F,
) -> std::result::Result<ProcessedFetchPage, String>
where
    F: FnOnce(std::thread::ThreadId) + Send + 'static,
{
    prober_cpu_with(limiter, move || {
        observe_decode_thread(std::thread::current().id());
        process_fetch_page(&body, event_id)
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_retention_work(
    prober_identity: &Identity,
    client: &Client,
    base_url: &Url,
    expected_key: &[u8; 32],
    wall_clock: u64,
    now: u64,
    mut result: ProbeResult,
    work: Option<RetentionWork>,
) -> ProbeOutcome {
    match work {
        None => ProbeOutcome {
            result,
            retention_update: None,
        },
        Some(RetentionWork::Create) => {
            let recipient = Identity::generate();
            let wrap = match envelope::wrap(
                prober_identity,
                &recipient.verifying_key(),
                &format!("pigeonpost retention canary {now}"),
                now,
            ) {
                Ok(wrap) => wrap,
                Err(_) => {
                    return failure(
                        &result.endpoint,
                        now,
                        true,
                        result.utilization,
                        "could not construct retention canary",
                    )
                }
            };
            let event_id = wrap.id();
            let publish: PublishResponse = match post_json(
                client,
                endpoint_url(base_url, "v1/publish"),
                &PublishRequest { wrap, token: None },
                MUTATION_BODY_LIMIT,
            )
            .await
            {
                Ok(response) => response,
                Err(detail) => {
                    return failure(&result.endpoint, now, true, result.utilization, detail)
                }
            };
            if publish.id != crate::entry::hex(&event_id) {
                return failure(
                    &result.endpoint,
                    now,
                    true,
                    result.utilization,
                    "loft acknowledged a different retention canary",
                );
            }
            ProbeOutcome {
                result,
                retention_update: Some(RetentionUpdate::Created {
                    recipient_seed: recipient.to_seed(),
                    event_id,
                    published_at: now,
                }),
            }
        }
        Some(RetentionWork::Check(canary)) => {
            let mut seed = canary.recipient_seed;
            let recipient = Identity::from_seed(seed);
            seed.zeroize();
            let age = now.saturating_sub(canary.published_at);
            result.retention_age_secs = Some(age);
            match fetch_event(
                client,
                base_url,
                &recipient,
                expected_key,
                wall_clock,
                canary.event_id,
            )
            .await
            {
                Ok(_) => {
                    result.retention_ok = Some(true);
                    ProbeOutcome {
                        result,
                        retention_update: Some(RetentionUpdate::Checked {
                            checked_at: now,
                            rotate: age >= canary.target_age_secs,
                        }),
                    }
                }
                Err(detail) => {
                    result.retention_ok = Some(false);
                    result.detail = Some(format!("retention canary check failed: {detail}"));
                    ProbeOutcome {
                        result,
                        // Do not advance the durable check time. The missing canary is retried on
                        // the next five-minute sweep, so three failures can actually degrade a
                        // retention-dishonest loft instead of being erased by healthy liveness
                        // probes between daily checks.
                        retention_update: None,
                    }
                }
            }
        }
    }
}

fn failure(
    endpoint: &str,
    now: u64,
    reachable: bool,
    utilization: f64,
    detail: impl Into<String>,
) -> ProbeOutcome {
    ProbeOutcome {
        result: ProbeResult {
            endpoint: endpoint.to_string(),
            at: now,
            reachable,
            stored_and_returned: false,
            utilization,
            retention_age_secs: None,
            retention_ok: None,
            detail: Some(detail.into()),
        },
        retention_update: None,
    }
}

async fn pinned_client(
    endpoint: &str,
    policy: NetworkPolicy,
) -> std::result::Result<(Client, Url), String> {
    let base_url = Url::parse(endpoint).map_err(|_| "loft endpoint is not a valid URL")?;
    if base_url.cannot_be_a_base()
        || base_url.host_str().is_none()
        || !base_url.username().is_empty()
        || base_url.password().is_some()
        || base_url.port() == Some(0)
        || base_url.query().is_some()
        || base_url.fragment().is_some()
        || !matches!(base_url.path(), "" | "/")
    {
        return Err("loft endpoint must be an origin without credentials, query, or path".into());
    }
    match base_url.scheme() {
        "https" => {}
        "http" if policy.allow_http => {}
        _ => return Err("loft endpoint must use HTTPS".into()),
    }

    let host = base_url.host_str().ok_or("loft endpoint has no host")?;
    if is_localhost_name(host) {
        return Err("loft endpoint cannot use localhost names".into());
    }
    let resolution_host = host.trim_start_matches('[').trim_end_matches(']');
    let port = base_url
        .port_or_known_default()
        .ok_or("loft endpoint has no port")?;
    let literal_ip = resolution_host.parse::<IpAddr>().ok();
    let addresses: Vec<_> = if let Some(address) = literal_ip {
        vec![SocketAddr::new(address, port)]
    } else {
        tokio::time::timeout(
            DNS_TIMEOUT,
            tokio::net::lookup_host((resolution_host, port)),
        )
        .await
        .map_err(|_| "loft endpoint DNS lookup timed out")?
        .map_err(|_| "loft endpoint DNS lookup failed")?
        .take(MAX_RESOLVED_ADDRESSES + 1)
        .collect()
    };
    if addresses.is_empty() {
        return Err("loft endpoint resolved to no addresses".into());
    }
    if addresses.len() > MAX_RESOLVED_ADDRESSES {
        return Err("loft endpoint resolved to too many addresses".into());
    }
    if !policy.allow_non_public_addresses
        && addresses.iter().any(|address| !is_public_ip(address.ip()))
    {
        return Err("loft endpoint resolved to a non-public address".into());
    }

    // Pin the hostname to the validated address for every request in this probe. TLS still checks
    // the original hostname, while a DNS change cannot redirect later publish/fetch calls.
    let pinned = SocketAddr::new(addresses[0].ip(), port);
    let mut builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .pool_max_idle_per_host(1);
    if literal_ip.is_none() {
        builder = builder.resolve(resolution_host, pinned);
    }
    let client = builder
        .build()
        .map_err(|_| "could not construct the bounded probe client")?;
    Ok((client, base_url))
}

fn endpoint_url(base: &Url, path: &str) -> Url {
    base.join(path)
        .expect("a validated origin URL always accepts a relative path")
}

async fn get_json<T: DeserializeOwned + Send + 'static>(
    client: &Client,
    url: Url,
    max_bytes: usize,
) -> std::result::Result<T, String> {
    receive_json(client.get(url), max_bytes).await
}

async fn post_json<B: serde::Serialize, T: DeserializeOwned + Send + 'static>(
    client: &Client,
    url: Url,
    body: &B,
    max_bytes: usize,
) -> std::result::Result<T, String> {
    receive_json(client.post(url).json(body), max_bytes).await
}

async fn receive_json<T: DeserializeOwned + Send + 'static>(
    request: RequestBuilder,
    max_bytes: usize,
) -> std::result::Result<T, String> {
    let body = receive_body(request, max_bytes).await?;
    prober_cpu(move || {
        serde_json::from_slice(&body).map_err(|_| "loft returned malformed JSON".to_owned())
    })
    .await
}

async fn receive_body(
    request: RequestBuilder,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, String> {
    let mut response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "loft request timed out"
        } else {
            "loft request failed"
        }
    })?;
    if !response.status().is_success() {
        return Err(format!("loft refused the probe ({})", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err("loft response exceeded the body limit".into());
    }

    let mut body =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(max_bytes as u64) as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "could not read the loft response")?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err("loft response exceeded the body limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Probe a fair, leased batch of due lofts with fixed concurrency and whole-sweep ceilings.
pub async fn sweep(directory: Arc<Directory>, identity: &Identity, now: u64) -> Result<usize> {
    sweep_with_policy(directory, identity, now, PRODUCTION_NETWORK_POLICY).await
}

#[cfg(feature = "test-utilities")]
#[doc(hidden)]
pub async fn sweep_for_test(
    directory: Arc<Directory>,
    identity: &Identity,
    now: u64,
) -> Result<usize> {
    sweep_with_policy(directory, identity, now, TEST_NETWORK_POLICY).await
}

async fn sweep_with_policy(
    directory: Arc<Directory>,
    identity: &Identity,
    now: u64,
    policy: NetworkPolicy,
) -> Result<usize> {
    let claim_directory = Arc::clone(&directory);
    let entries = blocking_directory(move || {
        claim_directory.claim_probe_candidates(now, MAX_PROBE_CANDIDATES)
    })
    .await?;

    let mut seed = identity.to_seed();
    let identity = Arc::new(Identity::from_seed(seed));
    seed.zeroize();
    let mut entries = entries.into_iter();
    let mut tasks = JoinSet::new();
    for _ in 0..MAX_CONCURRENT_PROBES {
        let Some(entry) = entries.next() else { break };
        spawn_probe(
            &mut tasks,
            Arc::clone(&identity),
            entry,
            now,
            policy,
            Arc::clone(&directory),
        )
        .await?;
    }

    let deadline = tokio::time::Instant::now() + SWEEP_DEADLINE;
    let mut probed = 0;
    loop {
        let joined = match tokio::time::timeout_at(deadline, tasks.join_next()).await {
            Ok(joined) => joined,
            Err(_) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                tracing::warn!(
                    probed,
                    kind = "probe_sweep_deadline",
                    "probe sweep hit its deadline"
                );
                return Ok(probed);
            }
        };
        let Some(joined) = joined else { break };
        let (expected_pubkey, expected_sequence, outcome) =
            joined.map_err(|_| DirectoryError::Io(std::io::Error::other("probe task failed")))?;
        let healthy = outcome.result.healthy();
        let record_directory = Arc::clone(&directory);
        let recorded = blocking_directory(move || {
            record_directory.record_claim_probe_with_retention(
                &outcome.result,
                now,
                outcome.retention_update,
                &expected_pubkey,
                expected_sequence,
            )
        })
        .await?;
        if recorded.is_none() {
            tracing::debug!(
                kind = "stale_probe",
                "discarded a probe for a replaced claim"
            );
        } else if !healthy {
            tracing::warn!(kind = "probe_failed", "probe failed");
        }
        probed += 1;
        if let Some(entry) = entries.next() {
            spawn_probe(
                &mut tasks,
                Arc::clone(&identity),
                entry,
                now,
                policy,
                Arc::clone(&directory),
            )
            .await?;
        }
    }
    Ok(probed)
}

async fn spawn_probe(
    tasks: &mut JoinSet<(String, u64, ProbeOutcome)>,
    identity: Arc<Identity>,
    entry: DirectoryEntry,
    now: u64,
    policy: NetworkPolicy,
    directory: Arc<Directory>,
) -> Result<()> {
    let retention_endpoint = entry.endpoint.clone();
    let retention_days = entry.retention_days;
    let retention_work = blocking_directory(move || {
        directory.retention_work(&retention_endpoint, retention_days, now)
    })
    .await?;
    tasks.spawn(async move {
        let outcome =
            probe_once_with_retention(&identity, &entry, now, policy, retention_work).await;
        (entry.pubkey, entry.sequence, outcome)
    });
    Ok(())
}

/// Probe on an interval until the caller requests a clean stop or durable state becomes unavailable.
pub async fn run(
    directory: Arc<Directory>,
    identity: Arc<Identity>,
    interval_secs: u64,
) -> Result<()> {
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    run_until(directory, identity, interval_secs, stop_rx).await
}

pub async fn run_until(
    directory: Arc<Directory>,
    identity: Arc<Identity>,
    interval_secs: u64,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if interval_secs == 0 {
        return Err(DirectoryError::Malformed(
            "probe interval must be nonzero".into(),
        ));
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return Ok(());
                }
                continue;
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let probed = sweep(Arc::clone(&directory), &identity, now).await?;
        let sweep_directory = Arc::clone(&directory);
        blocking_directory(move || sweep_directory.mark_probe_sweep(now)).await?;
        tracing::debug!(probed, "probe sweep complete");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use ed25519_dalek::SigningKey;

    use super::*;

    fn claimed_entry_and_info() -> (DirectoryEntry, InfoResponse, [u8; 32]) {
        let signing_key = SigningKey::from_bytes(&[0x42; 32]);
        let expected_key = signing_key.verifying_key().to_bytes();
        let entry = DirectoryEntry::signed(
            &signing_key,
            "https://loft.example",
            Some("/test/operator".to_owned()),
            100,
            30,
            crate::LoftPolicy {
                open: true,
                pow_floor: 18,
                max_event_bytes: 65_536,
            },
            0.0,
        );
        let info = InfoResponse {
            software: "pigeonpost-loft".to_owned(),
            version: "0.2.0".to_owned(),
            protocol: pigeonpost_core::PROTOCOL_VERSION.to_owned(),
            pubkey: crate::entry::hex(&expected_key),
            origin: "https://loft.example".to_owned(),
            capacity_bytes: 100 * GIB_BYTES,
            used_bytes: 25 * GIB_BYTES,
            utilization: 0.25,
            retention_days: 30,
            open: true,
            pow_floor: 18,
            max_event_bytes: 65_536,
            event_count: 7,
            accepting: true,
        };
        (entry, info, expected_key)
    }

    #[test]
    fn live_info_must_match_the_complete_signed_claim() {
        let (entry, info, expected_key) = claimed_entry_and_info();
        assert_eq!(
            validate_info_claim(&info, &entry, "https://loft.example", &expected_key),
            Ok(())
        );

        let mut mismatches = Vec::new();
        let mut mismatch = info.clone();
        mismatch.protocol = "pigeonpost/unsupported".to_owned();
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.origin = "https://other.example".to_owned();
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.pubkey = crate::entry::hex(&[0x24; 32]);
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.capacity_bytes += GIB_BYTES;
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.retention_days += 1;
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.open = false;
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.pow_floor += 1;
        mismatches.push(mismatch);
        let mut mismatch = info.clone();
        mismatch.max_event_bytes += 1;
        mismatches.push(mismatch);

        for mismatch in mismatches {
            assert!(
                validate_info_claim(&mismatch, &entry, "https://loft.example", &expected_key,)
                    .is_err(),
                "accepted a live claim that diverged from the signed directory entry: {mismatch:?}"
            );
        }
    }

    #[test]
    fn live_info_counters_must_authenticate_the_reported_utilization() {
        let (entry, info, expected_key) = claimed_entry_and_info();
        for mismatch in [
            InfoResponse {
                capacity_bytes: 0,
                used_bytes: 0,
                utilization: 0.0,
                ..info.clone()
            },
            InfoResponse {
                used_bytes: info.capacity_bytes + 1,
                ..info.clone()
            },
            InfoResponse {
                utilization: f64::NAN,
                ..info.clone()
            },
            InfoResponse {
                utilization: 0.5,
                ..info.clone()
            },
        ] {
            assert!(
                validate_info_claim(&mismatch, &entry, "https://loft.example", &expected_key,)
                    .is_err()
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_directory_work_is_bounded_without_starving_the_runtime() {
        let limiter = Arc::new(tokio::sync::Semaphore::new(1));
        let worker_limiter = Arc::clone(&limiter);
        let worker = tokio::spawn(async move {
            blocking_directory_with(worker_limiter, Duration::from_secs(1), || {
                std::thread::sleep(Duration::from_millis(75));
                Ok(())
            })
            .await
        });
        while limiter.available_permits() != 0 {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            blocking_directory_with(Arc::clone(&limiter), Duration::from_millis(10), || Ok(()))
                .await,
            Err(DirectoryError::Overloaded)
        ));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                tokio::time::sleep(Duration::from_millis(5)),
            )
            .await
            .is_ok(),
            "blocking directory work starved the single runtime thread"
        );
        worker.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prober_cpu_lane_fails_closed_and_counts_cancelled_work_until_exit() {
        let limiter = Arc::new(tokio::sync::Semaphore::new(1));
        let worker_limiter = Arc::clone(&limiter);
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            prober_cpu_with(worker_limiter, move || {
                let _ = started_tx.send(());
                while !worker_release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })
            .await
        });
        started_rx.await.unwrap();

        assert_eq!(
            prober_cpu_with(Arc::clone(&limiter), || Ok(())).await,
            Err(PROBER_CPU_BUSY.to_owned())
        );

        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        assert_eq!(
            limiter.available_permits(),
            0,
            "cancelling the async waiter released a still-running CPU operation"
        );

        release.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(1), async {
            while limiter.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("CPU permit was not recovered after the blocking operation exited");
    }

    fn hostile_fetch_page() -> Vec<u8> {
        fn wrap(marker: u8) -> pigeonpost_core::envelope::Wrap {
            pigeonpost_core::envelope::Wrap {
                version: pigeonpost_core::envelope::ENVELOPE_VERSION,
                ephemeral_pubkey: [marker; 32],
                recipient: [marker.wrapping_add(1); 32],
                nonce: [marker.wrapping_add(2); 24],
                ciphertext: vec![marker; 960 * 1024],
                created_at: u64::from(marker),
                signature: [marker.wrapping_add(3); 64],
                pow_nonce: 0,
                attribution: None,
            }
        }

        let body = serde_json::to_vec(&FetchResponse {
            events: vec![wrap(1), wrap(2)],
            next_cursor: 7,
            more: true,
        })
        .unwrap();
        assert!(body.len() > 3 * 1024 * 1024);
        assert!(body.len() <= FETCH_BODY_LIMIT);
        body
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maximal_hostile_fetch_pages_decode_in_the_bounded_cpu_lane() {
        let page = hostile_fetch_page();
        let pages = (0..MAX_FETCH_PAGES)
            .map(|_| page.clone())
            .collect::<Vec<_>>();
        let limiter = Arc::new(tokio::sync::Semaphore::new(1));
        let runtime_thread = std::thread::current().id();
        let decode_threads = Arc::new(std::sync::Mutex::new(Vec::new()));

        let worker_limiter = Arc::clone(&limiter);
        let worker_decode_threads = Arc::clone(&decode_threads);
        let worker = tokio::spawn(async move {
            for body in pages {
                let observed = Arc::clone(&worker_decode_threads);
                let processed = decode_fetch_page_with_observer(
                    Arc::clone(&worker_limiter),
                    body,
                    [0xff; 32],
                    move |thread| observed.lock().unwrap().push(thread),
                )
                .await?;
                assert!(processed.found.is_none());
                assert_eq!(processed.next_cursor, 7);
                assert!(processed.more);
            }
            Ok::<(), String>(())
        });
        tokio::time::timeout(Duration::from_secs(30), worker)
            .await
            .expect("bounded hostile fetch-page processing did not finish")
            .unwrap()
            .unwrap();
        let decode_threads = decode_threads.lock().unwrap();
        assert_eq!(decode_threads.len(), MAX_FETCH_PAGES);
        assert!(
            decode_threads
                .iter()
                .all(|thread| *thread != runtime_thread),
            "hostile response decoding ran on the current-thread Tokio scheduler"
        );
    }

    #[tokio::test]
    async fn supervised_prober_rejects_zero_interval_and_stops_cleanly() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        let identity = Arc::new(Identity::from_seed([3; 32]));
        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        assert!(matches!(
            run_until(Arc::clone(&directory), Arc::clone(&identity), 0, stop_rx).await,
            Err(DirectoryError::Malformed(_))
        ));

        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        stop_tx.send(true).unwrap();
        run_until(directory, identity, 60, stop_rx).await.unwrap();
    }

    #[test]
    fn production_rejects_every_special_ipv4_range() {
        for address in [
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            assert!(
                !is_public_ip(address.parse().unwrap()),
                "accepted {address}"
            );
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn production_rejects_special_ipv6_and_mapped_ipv4() {
        for address in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::192.168.1.1",
            "64:ff9b::10.0.0.1",
            "100::1",
            "100:0:0:1::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2002:0a00:0001::1",
            "3fff::1",
            "4000::1",
            "5f00::1",
        ] {
            assert!(
                !is_public_ip(address.parse().unwrap()),
                "accepted {address}"
            );
        }
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn production_rejects_http_before_making_a_request() {
        assert!(matches!(
            pinned_client("http://8.8.8.8", PRODUCTION_NETWORK_POLICY).await,
            Err(error) if error.contains("HTTPS")
        ));
    }

    #[tokio::test]
    async fn production_rejects_loopback_https() {
        assert!(matches!(
            pinned_client("https://127.0.0.1", PRODUCTION_NETWORK_POLICY).await,
            Err(error) if error.contains("non-public")
        ));
    }

    #[cfg(feature = "test-utilities")]
    #[tokio::test]
    async fn every_policy_rejects_localhost_names_and_port_zero_before_dns() {
        for endpoint in [
            "https://localhost",
            "https://localhost.",
            "https://api.localhost",
            "https://API.LOCALHOST.",
            "https://loft.example:0",
        ] {
            assert!(pinned_client(endpoint, TEST_NETWORK_POLICY).await.is_err());
        }
    }
}
