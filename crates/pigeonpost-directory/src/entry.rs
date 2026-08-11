//! Directory entries.
//!
//! **Entries are signed by the loft's own key.** The directory compiles submissions; it does not
//! author them. That single property removes the largest power a directory operator would
//! otherwise hold — inventing a loft, or inflating someone else's advertised capacity
//! (`docs/network.md` §Directory integrity).
//!
//! What the operator *can* still do is omit an entry, which is why submissions and removals are
//! appended to the transparency log, and mis-weight one, which is why the probe measurements the
//! weights are computed from are published and signed too.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use pigeonpost_core::keys;
use pigeonpost_registry::entry::{
    directory_add_claim_payload, directory_remove_claim_payload,
    DirectoryAdd as RegistryDirectoryAdd, DirectoryRemove as RegistryDirectoryRemove,
};
use serde::{Deserialize, Serialize};

use crate::error::{DirectoryError, Result};

/// Where a loft sits in its lifecycle (`docs/network.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoftState {
    /// Submitted, not yet probed clean for long enough.
    Pending,
    /// Selectable.
    Active,
    /// Failing probes. Existing agents keep using it; no new agent picks it.
    Degraded,
    /// Announced exit. Still serves reads until the drain date.
    Draining,
    /// Gone.
    Removed,
}

impl LoftState {
    /// Only `Active` attracts new agents. Everything else is either not ready or on the way out —
    /// and in both cases sending more agents there is the wrong move.
    pub fn selectable(self) -> bool {
        matches!(self, LoftState::Active)
    }
}

/// The acceptance rules a loft advertises, so a client can avoid one that would refuse its mail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoftPolicy {
    pub open: bool,
    pub pow_floor: u32,
    pub max_event_bytes: usize,
}

/// Observed, not claimed. Filled in by the prober and republished with the entry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Health {
    pub uptime_30d: f64,
    pub probe_fail_streak: u32,
    pub last_probe: u64,
}

/// What a loft submits, and what a client selects from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    pub endpoint: String,
    /// Hex Ed25519 key. Clients bind fetch proofs and token presentations to it.
    pub pubkey: String,
    /// Optional Pigeonpost handle. Binding a loft to an OIDC-backed identity offers
    /// accountability **without anyone gatekeeping admission** (`docs/network.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    pub capacity_gb: u64,
    pub retention_days: u64,
    pub policy: LoftPolicy,
    /// Monotonic operator mutation sequence. The directory accepts a mutation only when this is
    /// strictly greater than the last sequence stored for the endpoint.
    ///
    /// Zero identifies a legacy v1 entry. It is accepted only for an endpoint's first registration;
    /// every later mutation must use a strictly increasing v2 sequence.
    #[serde(default)]
    pub sequence: u64,
    /// Self-reported and gameable only in the safe direction: understating loses you traffic.
    #[serde(default)]
    pub utilization: f64,
    /// Signature by the loft's key over everything above.
    pub signature: String,

    // ---- filled in by the directory, not the submitter ----
    #[serde(default = "default_state")]
    pub state: LoftState,
    #[serde(default)]
    pub health: Health,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_after: Option<u64>,
    /// Latest sequence accepted for either submit or drain. Unlike `sequence`, this is a directory
    /// observation and is covered by the signed directory document rather than the loft entry.
    #[serde(default)]
    pub last_mutation_sequence: u64,
}

fn default_state() -> LoftState {
    LoftState::Pending
}

impl DirectoryEntry {
    /// Build and sign an entry. Run by the loft operator, never by the directory.
    #[allow(clippy::too_many_arguments)]
    pub fn signed(
        key: &SigningKey,
        endpoint: &str,
        operator: Option<String>,
        capacity_gb: u64,
        retention_days: u64,
        policy: LoftPolicy,
        utilization: f64,
    ) -> Self {
        Self::signed_with_sequence(
            key,
            endpoint,
            operator,
            capacity_gb,
            retention_days,
            policy,
            utilization,
            1,
        )
    }

    /// Build and sign a mutation at an explicit monotonic sequence.
    #[allow(clippy::too_many_arguments)]
    pub fn signed_with_sequence(
        key: &SigningKey,
        endpoint: &str,
        operator: Option<String>,
        capacity_gb: u64,
        retention_days: u64,
        policy: LoftPolicy,
        utilization: f64,
        sequence: u64,
    ) -> Self {
        let pubkey = hex(key.verifying_key().as_bytes());
        let payload = payload_v2(
            endpoint,
            &pubkey,
            operator.as_deref(),
            capacity_gb,
            retention_days,
            &policy,
            sequence,
        );

        DirectoryEntry {
            endpoint: endpoint.to_string(),
            pubkey,
            operator,
            capacity_gb,
            retention_days,
            policy,
            sequence,
            utilization,
            signature: hex(&key.sign(&payload).to_bytes()),
            state: LoftState::Pending,
            health: Health::default(),
            drain_after: None,
            last_mutation_sequence: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn signed_legacy_for_test(
        key: &SigningKey,
        endpoint: &str,
        capacity_gb: u64,
        retention_days: u64,
        policy: LoftPolicy,
    ) -> Self {
        let pubkey = hex(key.verifying_key().as_bytes());
        let signature = hex(&key
            .sign(&payload_v1(
                endpoint,
                &pubkey,
                None,
                capacity_gb,
                retention_days,
                &policy,
            ))
            .to_bytes());
        Self {
            endpoint: endpoint.to_string(),
            pubkey,
            operator: None,
            capacity_gb,
            retention_days,
            policy,
            sequence: 0,
            utilization: 0.0,
            signature,
            state: LoftState::Pending,
            health: Health::default(),
            drain_after: None,
            last_mutation_sequence: 0,
        }
    }

    /// Verify the entry is signed by the key it names.
    ///
    /// `utilization`, `state`, `health`, `drain_after`, and `last_mutation_sequence` are
    /// deliberately outside the signature: they are the directory's observations, not the loft's
    /// claims. The things that would let a directory forge a *different loft* — endpoint, key,
    /// capacity, retention, policy — are all covered.
    pub fn verify(&self) -> Result<VerifyingKey> {
        let pubkey_bytes = parse_hex32(&self.pubkey)
            .ok_or_else(|| DirectoryError::Malformed("pubkey must be 32 hex bytes".into()))?;
        let key = keys::verifying_key_from_bytes(&pubkey_bytes)
            .map_err(|_| DirectoryError::Malformed("pubkey is not a valid key".into()))?;
        let signature_bytes = parse_hex64(&self.signature)
            .ok_or_else(|| DirectoryError::Malformed("signature must be 64 hex bytes".into()))?;

        let payload = if self.sequence == 0 {
            payload_v1(
                &self.endpoint,
                &self.pubkey,
                self.operator.as_deref(),
                self.capacity_gb,
                self.retention_days,
                &self.policy,
            )
        } else {
            payload_v2(
                &self.endpoint,
                &self.pubkey,
                self.operator.as_deref(),
                self.capacity_gb,
                self.retention_days,
                &self.policy,
                self.sequence,
            )
        };

        keys::verify(&key, &payload, &Signature::from_bytes(&signature_bytes))
            .map_err(|_| DirectoryError::BadSignature)?;
        Ok(key)
    }

    /// Exact authenticated mutation submitted to the shared registry log. Directory observations
    /// such as utilization and health are deliberately excluded.
    pub fn registry_addition(&self) -> Result<RegistryDirectoryAdd> {
        self.verify()?;
        RegistryDirectoryAdd::authenticated(
            self.endpoint.clone(),
            self.pubkey.clone(),
            self.operator.clone(),
            self.capacity_gb,
            self.retention_days,
            self.policy.open,
            self.policy.pow_floor,
            self.policy.max_event_bytes as u64,
            self.sequence,
            self.signature.clone(),
        )
        .map_err(|error| DirectoryError::Malformed(error.to_string()))
    }

    /// Free capacity in bytes, as advertised. The basis of selection weight.
    pub fn free_bytes(&self) -> u64 {
        let total = self.capacity_gb.saturating_mul(1024 * 1024 * 1024);
        let used = (total as f64 * self.utilization.clamp(0.0, 1.0)) as u64;
        total.saturating_sub(used)
    }

    /// Selection weight: free capacity discounted by how reliable the node has actually been.
    ///
    /// Over-advertising is self-correcting rather than policed — a loft that claims more than it
    /// has fills up, starts refusing, and the prober drops its uptime within one cycle.
    pub fn weight(&self) -> f64 {
        if !self.state.selectable() {
            return 0.0;
        }
        let uptime = if self.health.last_probe == 0 {
            1.0 // never probed yet: do not punish a fresh node into invisibility
        } else {
            self.health.uptime_30d.clamp(0.0, 1.0)
        };
        self.free_bytes() as f64 * uptime
    }

    /// Authenticated and advisory failure domains used by client diversity selection.
    ///
    /// The endpoint host is always included because the loft's signed entry and successful TLS
    /// probe bind the loft to that host. `operator` is only a self-asserted label today: including
    /// it can collapse lofts across hosts, but it must never replace the host and let two ports or
    /// labels on one machine masquerade as independent failure domains.
    pub fn failure_domains(&self) -> Vec<String> {
        let mut domains = vec![format!("host:{}", host_of(&self.endpoint))];
        if let Some(operator) = &self.operator {
            domains.push(format!("operator:{operator}"));
        }
        domains
    }
}

/// A graceful-exit mutation, authorized by the same key as the directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainAuthorization {
    pub endpoint: String,
    pub after: u64,
    pub sequence: u64,
    pub signature: String,
}

impl DrainAuthorization {
    pub fn signed(key: &SigningKey, endpoint: &str, after: u64, sequence: u64) -> Self {
        let payload = drain_payload(endpoint, after, sequence);
        Self {
            endpoint: endpoint.to_string(),
            after,
            sequence,
            signature: hex(&key.sign(&payload).to_bytes()),
        }
    }

    pub fn verify(&self, key: &VerifyingKey) -> Result<()> {
        let signature = parse_hex64(&self.signature)
            .ok_or_else(|| DirectoryError::Malformed("signature must be 64 hex bytes".into()))?;
        keys::verify(
            key,
            &drain_payload(&self.endpoint, self.after, self.sequence),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| DirectoryError::BadSignature)
    }

    /// Exact authenticated mutation submitted to the shared registry log.
    pub fn registry_removal(&self, loft_pubkey: String) -> RegistryDirectoryRemove {
        RegistryDirectoryRemove::authenticated(
            self.endpoint.clone(),
            loft_pubkey,
            self.after,
            self.sequence,
            self.signature.clone(),
        )
    }
}

fn payload_v1(
    endpoint: &str,
    pubkey: &str,
    operator: Option<&str>,
    capacity_gb: u64,
    retention_days: u64,
    policy: &LoftPolicy,
) -> Vec<u8> {
    directory_add_claim_payload(
        endpoint,
        pubkey,
        operator,
        capacity_gb,
        retention_days,
        policy.open,
        policy.pow_floor,
        policy.max_event_bytes as u64,
        0,
    )
    .expect("validated directory fields fit the canonical codec")
}

fn payload_v2(
    endpoint: &str,
    pubkey: &str,
    operator: Option<&str>,
    capacity_gb: u64,
    retention_days: u64,
    policy: &LoftPolicy,
    sequence: u64,
) -> Vec<u8> {
    directory_add_claim_payload(
        endpoint,
        pubkey,
        operator,
        capacity_gb,
        retention_days,
        policy.open,
        policy.pow_floor,
        policy.max_event_bytes as u64,
        sequence,
    )
    .expect("validated directory fields fit the canonical codec")
}

fn drain_payload(endpoint: &str, after: u64, sequence: u64) -> Vec<u8> {
    directory_remove_claim_payload(endpoint, after, sequence)
        .expect("validated directory fields fit the canonical codec")
}

/// Host portion of a URL, for the diversity fallback.
fn host_of(endpoint: &str) -> String {
    endpoint
        .split("://")
        .nth(1)
        .unwrap_or(endpoint)
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .split(':')
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex32(input: &str) -> Option<[u8; 32]> {
    parse_hex(input, 32)?.try_into().ok()
}

pub(crate) fn parse_hex64(input: &str) -> Option<[u8; 64]> {
    parse_hex(input, 64)?.try_into().ok()
}

fn parse_hex(input: &str, len: usize) -> Option<Vec<u8>> {
    if input.len() != len * 2 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for chunk in input.as_bytes().chunks(2) {
        out.push(u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> LoftPolicy {
        LoftPolicy {
            open: true,
            pow_floor: 18,
            max_event_bytes: 65536,
        }
    }

    fn entry(seed: u8, endpoint: &str, capacity_gb: u64) -> DirectoryEntry {
        DirectoryEntry::signed(
            &SigningKey::from_bytes(&[seed; 32]),
            endpoint,
            None,
            capacity_gb,
            30,
            policy(),
            0.0,
        )
    }

    #[test]
    fn a_signed_entry_verifies() {
        assert!(entry(1, "wss://a.example", 100).verify().is_ok());
    }

    #[test]
    fn a_legacy_v1_entry_without_a_sequence_still_verifies() {
        let key = SigningKey::from_bytes(&[1; 32]);
        let mut legacy = entry(1, "wss://a.example", 100);
        legacy.sequence = 0;
        legacy.signature = hex(&key
            .sign(&payload_v1(
                &legacy.endpoint,
                &legacy.pubkey,
                legacy.operator.as_deref(),
                legacy.capacity_gb,
                legacy.retention_days,
                &legacy.policy,
            ))
            .to_bytes());
        assert!(legacy.verify().is_ok());
    }

    #[test]
    fn the_directory_cannot_invent_a_loft() {
        // The point of self-signing: a compiled entry nobody's key signed is invalid.
        let mut forged = entry(1, "wss://a.example", 100);
        forged.endpoint = "wss://attacker.example".into();
        assert!(matches!(forged.verify(), Err(DirectoryError::BadSignature)));
    }

    #[test]
    fn the_directory_cannot_inflate_advertised_capacity() {
        let mut inflated = entry(1, "wss://a.example", 100);
        inflated.capacity_gb = 100_000;
        assert!(inflated.verify().is_err());
    }

    #[test]
    fn the_sequence_is_covered_by_the_entry_signature() {
        let mut changed = entry(1, "wss://a.example", 100);
        changed.sequence += 1;
        assert!(matches!(
            changed.verify(),
            Err(DirectoryError::BadSignature)
        ));
    }

    #[test]
    fn drain_authorization_covers_every_mutable_field() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let authorization = DrainAuthorization::signed(&key, "wss://a.example", 50, 2);
        assert!(authorization.verify(&key.verifying_key()).is_ok());

        let mut changed_after = authorization.clone();
        changed_after.after = 51;
        let mut changed_sequence = authorization.clone();
        changed_sequence.sequence += 1;
        for tampered in [changed_after, changed_sequence] {
            assert!(matches!(
                tampered.verify(&key.verifying_key()),
                Err(DirectoryError::BadSignature)
            ));
        }
    }

    #[test]
    fn the_directory_may_update_its_own_observations() {
        // These are the directory's measurements, not the loft's claims, so they are outside the
        // signature by design.
        let mut observed = entry(1, "wss://a.example", 100);
        observed.state = LoftState::Active;
        observed.utilization = 0.5;
        observed.health = Health {
            uptime_30d: 0.99,
            probe_fail_streak: 0,
            last_probe: 1_786_105_721,
        };
        assert!(observed.verify().is_ok());
    }

    #[test]
    fn only_active_lofts_have_weight() {
        let mut e = entry(1, "wss://a.example", 100);
        for state in [
            LoftState::Pending,
            LoftState::Degraded,
            LoftState::Draining,
            LoftState::Removed,
        ] {
            e.state = state;
            assert_eq!(e.weight(), 0.0, "{state:?} must not attract new agents");
        }
        e.state = LoftState::Active;
        assert!(e.weight() > 0.0);
    }

    #[test]
    fn weight_falls_as_a_loft_fills() {
        let mut empty = entry(1, "wss://a.example", 100);
        empty.state = LoftState::Active;
        let mut full = empty.clone();
        full.utilization = 0.9;

        assert!(full.weight() < empty.weight());
    }

    #[test]
    fn weight_falls_with_poor_uptime() {
        let mut reliable = entry(1, "wss://a.example", 100);
        reliable.state = LoftState::Active;
        reliable.health.last_probe = 1;
        reliable.health.uptime_30d = 1.0;

        let mut flaky = reliable.clone();
        flaky.health.uptime_30d = 0.5;

        assert!(flaky.weight() < reliable.weight());
    }

    #[test]
    fn lofts_on_one_host_are_one_failure_domain_even_with_different_labels() {
        let a = entry(1, "wss://box.example:7717", 100);
        let mut b = entry(2, "wss://box.example:7718", 100);
        b.operator = Some("/github/claimed-elsewhere".into());
        assert_eq!(a.failure_domains()[0], b.failure_domains()[0]);
    }

    #[test]
    fn a_declared_operator_only_adds_a_failure_domain() {
        let mut a = entry(1, "wss://a.example", 100);
        a.operator = Some("/github/someorg".into());
        assert_eq!(
            a.failure_domains(),
            vec!["host:a.example", "operator:/github/someorg"]
        );
    }
}
