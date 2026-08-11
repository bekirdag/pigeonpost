//! Bounded request admission.
//!
//! A recipient bucket alone is a denial-of-service primitive: an attacker can rotate recipient
//! bytes forever, or spend a victim's allowance on malformed traffic. Admission therefore has a
//! global budget, an optional connected-source budget, and a recipient budget charged only after
//! the public envelope has verified. Keyed maps have a hard cardinality ceiling.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::LoftConfig;
use crate::error::{LoftError, Result};

const WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct Limits {
    requests: u32,
    bytes: u64,
}

#[derive(Clone, Copy)]
struct Bucket {
    requests: u32,
    bytes: u64,
    window_start: Instant,
}

impl Bucket {
    fn fresh(now: Instant) -> Self {
        Self {
            requests: 0,
            bytes: 0,
            window_start: now,
        }
    }

    fn charge(&mut self, now: Instant, bytes: u64, limits: Limits) -> Result<()> {
        if now.duration_since(self.window_start) >= WINDOW {
            *self = Self::fresh(now);
        }

        let next_requests = self.requests.saturating_add(1);
        let next_bytes = self.bytes.saturating_add(bytes);
        if (limits.requests != 0 && next_requests > limits.requests)
            || (limits.bytes != 0 && next_bytes > limits.bytes)
        {
            return Err(LoftError::RateLimited);
        }
        self.requests = next_requests;
        self.bytes = next_bytes;
        Ok(())
    }

    fn charge_bytes(&mut self, now: Instant, bytes: u64, limit: u64) -> Result<()> {
        if now.duration_since(self.window_start) >= WINDOW {
            *self = Self::fresh(now);
        }

        let next_bytes = self.bytes.saturating_add(bytes);
        if limit != 0 && next_bytes > limit {
            return Err(LoftError::RateLimited);
        }
        self.bytes = next_bytes;
        Ok(())
    }
}

struct KeyedBuckets<K> {
    state: Mutex<KeyedBucketState<K>>,
    max_keys: usize,
}

struct KeyedBucketState<K> {
    buckets: HashMap<K, Bucket>,
    next_cleanup_at: Option<Instant>,
    #[cfg(test)]
    cleanup_scans: usize,
}

impl<K: Copy + Eq + Hash> KeyedBuckets<K> {
    fn new(max_keys: usize) -> Self {
        Self {
            state: Mutex::new(KeyedBucketState {
                buckets: HashMap::new(),
                next_cleanup_at: None,
                #[cfg(test)]
                cleanup_scans: 0,
            }),
            max_keys,
        }
    }

    fn make_room(
        state: &mut KeyedBucketState<K>,
        key: &K,
        max_keys: usize,
        now: Instant,
    ) -> Result<()> {
        if state.buckets.contains_key(key) || state.buckets.len() < max_keys {
            return Ok(());
        }
        // A full live map is the normal fail-closed state. Do not rescan all attacker-controlled
        // keys for every distinct miss; scan only once the oldest possible window can expire.
        if state
            .next_cleanup_at
            .is_some_and(|deadline| now >= deadline)
        {
            state
                .buckets
                .retain(|_, bucket| now.duration_since(bucket.window_start) < WINDOW);
            // One full scan per rate window is enough. Recomputing the exact next staggered
            // expiry could still permit one O(N) scan per arriving key; a conservative delay in
            // reusing expired slots is safer than scheduler amplification.
            state.next_cleanup_at = (!state.buckets.is_empty()).then_some(now + WINDOW);
            #[cfg(test)]
            {
                state.cleanup_scans += 1;
            }
        }
        if state.buckets.len() >= max_keys {
            return Err(LoftError::RateLimited);
        }
        Ok(())
    }

    fn note_insert(state: &mut KeyedBucketState<K>, now: Instant) {
        let expiry = now + WINDOW;
        state.next_cleanup_at = Some(
            state
                .next_cleanup_at
                .map_or(expiry, |earliest| earliest.min(expiry)),
        );
    }

    fn charge(&self, key: K, bytes: u64, limits: Limits) -> Result<()> {
        if limits.requests == 0 && limits.bytes == 0 {
            return Ok(());
        }
        if self.max_keys == 0 {
            return Err(LoftError::RateLimited);
        }

        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let inserting = !state.buckets.contains_key(&key);
        Self::make_room(&mut state, &key, self.max_keys, now)?;
        if inserting {
            Self::note_insert(&mut state, now);
        }
        state
            .buckets
            .entry(key)
            .or_insert_with(|| Bucket::fresh(now))
            .charge(now, bytes, limits)
    }

    fn charge_bytes(&self, key: K, bytes: u64, limit: u64) -> Result<()> {
        if limit == 0 {
            return Ok(());
        }
        if self.max_keys == 0 {
            return Err(LoftError::RateLimited);
        }

        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let inserting = !state.buckets.contains_key(&key);
        Self::make_room(&mut state, &key, self.max_keys, now)?;
        if inserting {
            Self::note_insert(&mut state, now);
        }
        state
            .buckets
            .entry(key)
            .or_insert_with(|| Bucket::fresh(now))
            .charge_bytes(now, bytes, limit)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buckets
            .len()
    }

    #[cfg(test)]
    fn cleanup_scans(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cleanup_scans
    }
}

/// Owns every in-memory admission budget. The semaphore is non-waiting: once all permits are in
/// use, a new request is rejected instead of creating an unbounded queue of buffered bodies.
pub struct AdmissionController {
    concurrency: Arc<Semaphore>,
    global: Mutex<Bucket>,
    global_limits: Limits,
    source: KeyedBuckets<IpAddr>,
    source_limits: Limits,
    recipient: KeyedBuckets<[u8; 32]>,
    recipient_limits: Limits,
}

pub struct AdmissionPermit {
    _permit: OwnedSemaphorePermit,
}

impl AdmissionController {
    pub fn new(config: &LoftConfig) -> Self {
        Self {
            concurrency: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            global: Mutex::new(Bucket::fresh(Instant::now())),
            global_limits: Limits {
                requests: config.global_requests_per_minute,
                bytes: config.global_bytes_per_minute,
            },
            source: KeyedBuckets::new(config.max_limiter_keys),
            source_limits: Limits {
                requests: config.source_requests_per_minute,
                bytes: config.source_bytes_per_minute,
            },
            recipient: KeyedBuckets::new(config.max_limiter_keys),
            recipient_limits: Limits {
                requests: config.rate_limit_per_minute,
                bytes: config.recipient_bytes_per_minute,
            },
        }
    }

    pub fn try_enter(&self) -> Result<AdmissionPermit> {
        self.concurrency
            .clone()
            .try_acquire_owned()
            .map(|permit| AdmissionPermit { _permit: permit })
            .map_err(|_| LoftError::Overloaded)
    }

    /// Charge the budgets that cannot be bypassed by rotating recipient keys.
    pub fn charge_shared(&self, source: Option<IpAddr>, bytes: u64) -> Result<()> {
        self.global
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .charge(Instant::now(), bytes, self.global_limits)?;
        if let Some(source) = source {
            self.source.charge(source, bytes, self.source_limits)?;
        }
        Ok(())
    }

    /// Charge only after public structure and the outer signature have verified.
    pub fn charge_recipient(&self, recipient: &[u8; 32], bytes: u64) -> Result<()> {
        self.recipient
            .charge(*recipient, bytes, self.recipient_limits)
    }

    /// Debit the exact serialized response size without manufacturing another request count.
    /// Charges are intentionally not rolled back if a later keyed bucket rejects: conservative
    /// accounting prevents an attacker from probing one budget for free through another.
    pub fn charge_egress(
        &self,
        source: Option<IpAddr>,
        recipient: &[u8; 32],
        bytes: u64,
    ) -> Result<()> {
        self.global
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .charge_bytes(Instant::now(), bytes, self.global_limits.bytes)?;
        if let Some(source) = source {
            self.source
                .charge_bytes(source, bytes, self.source_limits.bytes)?;
        }
        self.recipient
            .charge_bytes(*recipient, bytes, self.recipient_limits.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LoftConfig {
        let mut config = LoftConfig::new([0; 32], "http://127.0.0.1:1");
        config.rate_limit_per_minute = 2;
        config.recipient_bytes_per_minute = 10;
        config.global_requests_per_minute = 3;
        config.global_bytes_per_minute = 30;
        config.source_requests_per_minute = 2;
        config.source_bytes_per_minute = 20;
        config.max_limiter_keys = 2;
        config.max_concurrent_requests = 1;
        config
    }

    #[test]
    fn recipient_is_charged_by_requests_and_bytes() {
        let admission = AdmissionController::new(&config());
        assert!(admission.charge_recipient(&[1; 32], 5).is_ok());
        assert!(admission.charge_recipient(&[1; 32], 5).is_ok());
        assert!(matches!(
            admission.charge_recipient(&[1; 32], 1),
            Err(LoftError::RateLimited)
        ));
    }

    #[test]
    fn rotating_recipients_cannot_bypass_the_global_budget() {
        let admission = AdmissionController::new(&config());
        for _ in 0..3 {
            assert!(admission.charge_shared(None, 1).is_ok());
        }
        assert!(admission.charge_shared(None, 1).is_err());
    }

    #[test]
    fn shared_byte_budget_is_independent_of_recipient() {
        let mut config = config();
        config.global_requests_per_minute = 100;
        config.global_bytes_per_minute = 10;
        let admission = AdmissionController::new(&config);
        assert!(admission.charge_shared(None, 6).is_ok());
        assert!(matches!(
            admission.charge_shared(None, 5),
            Err(LoftError::RateLimited)
        ));
    }

    #[test]
    fn attacker_controlled_maps_never_exceed_the_cap() {
        let admission = AdmissionController::new(&config());
        assert!(admission.charge_recipient(&[1; 32], 1).is_ok());
        assert!(admission.charge_recipient(&[2; 32], 1).is_ok());
        assert!(admission.charge_recipient(&[3; 32], 1).is_err());
        assert_eq!(admission.recipient.len(), 2);
    }

    #[test]
    fn full_live_key_map_rejects_new_keys_without_repeated_scans() {
        let buckets = KeyedBuckets::new(2);
        let limits = Limits {
            requests: 10,
            bytes: 10,
        };
        buckets.charge(1u32, 1, limits).unwrap();
        buckets.charge(2u32, 1, limits).unwrap();

        for key in 3..10_000u32 {
            assert!(matches!(
                buckets.charge(key, 1, limits),
                Err(LoftError::RateLimited)
            ));
        }
        assert_eq!(buckets.cleanup_scans(), 0);

        {
            let mut state = buckets
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let expired = Instant::now() - WINDOW;
            state.buckets.get_mut(&1).unwrap().window_start = expired;
            state.next_cleanup_at = Some(expired + WINDOW);
        }
        buckets.charge(10_000u32, 1, limits).unwrap();
        assert_eq!(buckets.cleanup_scans(), 1);
        assert_eq!(buckets.len(), 2);
        for key in 10_001..20_000u32 {
            assert!(matches!(
                buckets.charge(key, 1, limits),
                Err(LoftError::RateLimited)
            ));
        }
        assert_eq!(buckets.cleanup_scans(), 1);
    }

    #[test]
    fn concurrency_rejects_instead_of_queueing() {
        let admission = AdmissionController::new(&config());
        let first = admission.try_enter().unwrap();
        assert!(matches!(admission.try_enter(), Err(LoftError::Overloaded)));
        drop(first);
        assert!(admission.try_enter().is_ok());
    }

    #[test]
    fn exact_egress_bytes_do_not_consume_request_counts() {
        let mut config = config();
        config.global_requests_per_minute = 1;
        config.source_requests_per_minute = 1;
        config.rate_limit_per_minute = 1;
        let admission = AdmissionController::new(&config);
        let source = "192.0.2.10".parse().unwrap();
        let recipient = [7; 32];

        admission
            .charge_egress(Some(source), &recipient, 5)
            .unwrap();
        admission.charge_shared(Some(source), 1).unwrap();
        admission.charge_recipient(&recipient, 1).unwrap();
    }

    #[test]
    fn egress_is_bounded_globally_by_source_and_by_recipient() {
        let source = "192.0.2.10".parse().unwrap();
        let recipient = [7; 32];

        let mut global_config = config();
        global_config.global_bytes_per_minute = 4;
        let global = AdmissionController::new(&global_config);
        assert!(matches!(
            global.charge_egress(Some(source), &recipient, 5),
            Err(LoftError::RateLimited)
        ));

        let mut source_config = config();
        source_config.global_bytes_per_minute = 100;
        source_config.source_bytes_per_minute = 4;
        let by_source = AdmissionController::new(&source_config);
        assert!(matches!(
            by_source.charge_egress(Some(source), &recipient, 5),
            Err(LoftError::RateLimited)
        ));

        let mut recipient_config = config();
        recipient_config.global_bytes_per_minute = 100;
        recipient_config.source_bytes_per_minute = 100;
        recipient_config.recipient_bytes_per_minute = 4;
        let by_recipient = AdmissionController::new(&recipient_config);
        assert!(matches!(
            by_recipient.charge_egress(Some(source), &recipient, 5),
            Err(LoftError::RateLimited)
        ));
    }
}
