//! Signed tree heads, in the C2SP signed-note format.
//!
//! Deliberately not a bespoke JSON blob: `docs/architecture.md` chose RFC 6962 and the
//! `tlog-witness` model so independently operated services can consume it. Emitting the format
//! existing witness tooling parses is most of what makes that true.
//!
//! ```text
//! pigeonpost.dev/registry
//! 48211
//! 3q2+7wAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
//!
//! — pigeonpost.dev/registry BASE64(keyhash ‖ signature)
//! ```

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{RegistryError, Result};
use crate::log::Hash;

/// Ed25519, as numbered by the C2SP note spec.
const ALG_ED25519: u8 = 0x01;
/// Ed25519 timestamped transparency-log cosignature, per C2SP tlog-cosignature v1.
const ALG_ED25519_COSIGNATURE: u8 = 0x04;

/// Whether a nonempty witness policy guarantees that any two accepted quorums intersect.
///
/// Division is used instead of `2 * threshold` so untrusted configuration cannot overflow. This
/// property applies to one shared witness roster. Preventing divergent acceptance additionally
/// requires at least one non-equivocating witness in every quorum intersection; independently
/// configured rosters need a guaranteed honest overlap or an external gossip/coordination layer.
pub const fn witness_quorum_intersects(threshold: usize, witness_count: usize) -> bool {
    threshold != 0 && threshold <= witness_count && threshold > witness_count / 2
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub origin: String,
    pub size: u64,
    pub root: Hash,
}

/// One independently pinned C2SP note signer.
///
/// The registry key proves that a checkpoint came from the log operator. Witness keys prove that
/// independently operated observers saw the same append-only history. Callers choose the witness
/// threshold; merely accepting extra, unverified signature lines would not provide that property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessKey {
    name: String,
    key: VerifyingKey,
}

/// A checkpoint whose operator signature and configured witness policy were verified.
///
/// `witnessed_at` is the timestamp at which the configured quorum was most recently satisfied:
/// the oldest timestamp among the freshest `threshold` valid cosignatures. It is `None` only when
/// the caller explicitly configured a zero-witness policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCheckpoint {
    pub checkpoint: Checkpoint,
    pub witnessed_at: Option<u64>,
}

impl WitnessKey {
    pub fn new(name: impl Into<String>, key: VerifyingKey) -> Result<Self> {
        let name = name.into();
        if name.is_empty()
            || name.len() > 256
            || name
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(RegistryError::MalformedCheckpoint(
                "witness name is malformed".into(),
            ));
        }
        if key.is_weak() {
            return Err(RegistryError::InvalidConfiguration(
                "invalid registry witness key".into(),
            ));
        }
        Ok(Self { name, key })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn key(&self) -> &VerifyingKey {
        &self.key
    }
}

impl Checkpoint {
    /// The body that gets signed: origin, size, root, each on its own line.
    pub fn body(&self) -> String {
        format!("{}\n{}\n{}\n", self.origin, self.size, b64(&self.root))
    }

    /// Body plus signature line.
    pub fn sign(&self, key: &SigningKey) -> String {
        let body = self.body();
        let signature = key.sign(body.as_bytes());
        let keyhash = key_hash(&self.origin, &key.verifying_key());

        let mut blob = Vec::with_capacity(4 + 64);
        blob.extend_from_slice(&keyhash);
        blob.extend_from_slice(&signature.to_bytes());

        format!("{body}\n— {} {}\n", self.origin, b64(&blob))
    }

    /// Produce one C2SP tlog-cosignature v1 line for this checkpoint.
    ///
    /// Witness software appends the returned line to the operator-signed note after it has
    /// independently verified append-only consistency. The timestamp is seconds since the Unix
    /// epoch and is part of both the encoded signature and the signed message.
    pub fn cosignature_line(
        &self,
        witness_name: &str,
        witness_key: &SigningKey,
        timestamp: u64,
    ) -> Result<String> {
        WitnessKey::new(witness_name, witness_key.verifying_key())?;
        if timestamp > i64::MAX as u64 {
            return Err(RegistryError::MalformedCheckpoint(
                "cosignature timestamp exceeds the C2SP limit".into(),
            ));
        }
        let body = self.body();
        let message = format!("cosignature/v1\ntime {timestamp}\n{body}");
        let signature = witness_key.sign(message.as_bytes());
        let mut blob = Vec::with_capacity(4 + 8 + 64);
        blob.extend_from_slice(&cosignature_key_hash(
            witness_name,
            &witness_key.verifying_key(),
        ));
        blob.extend_from_slice(&timestamp.to_be_bytes());
        blob.extend_from_slice(&signature.to_bytes());
        Ok(format!("— {witness_name} {}\n", b64(&blob)))
    }

    /// Parse and verify a signed checkpoint.
    ///
    /// This is the function a witness or a client runs. It is here, rather than only in a test,
    /// because "anyone can verify" has to mean shipped code.
    pub fn verify(text: &str, key: &VerifyingKey) -> Result<Checkpoint> {
        if text.len() > 64 * 1024 {
            return Err(RegistryError::MalformedCheckpoint(
                "checkpoint exceeds 64 KiB".into(),
            ));
        }
        let (body, signatures) = text
            .split_once("\n\n")
            .ok_or_else(|| RegistryError::MalformedCheckpoint("missing signature block".into()))?;

        let mut lines = body.lines();
        let origin = lines
            .next()
            .ok_or_else(|| RegistryError::MalformedCheckpoint("no origin".into()))?
            .to_string();
        let size: u64 = lines
            .next()
            .ok_or_else(|| RegistryError::MalformedCheckpoint("no size".into()))?
            .parse()
            .map_err(|_| RegistryError::MalformedCheckpoint("size is not a number".into()))?;
        let root_b64 = lines
            .next()
            .ok_or_else(|| RegistryError::MalformedCheckpoint("no root".into()))?;
        if lines.next().is_some() {
            return Err(RegistryError::MalformedCheckpoint(
                "checkpoint body has extra fields".into(),
            ));
        }

        let root: Hash = unb64(root_b64)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| RegistryError::MalformedCheckpoint("root is not 32 bytes".into()))?;

        if !has_valid_signature(body, signatures, &origin, key) {
            return Err(RegistryError::MalformedCheckpoint(
                "no valid signature from the expected key".into(),
            ));
        }

        Ok(Checkpoint { origin, size, root })
    }

    /// Verify the registry signature and a threshold of independently pinned witness signatures.
    ///
    /// Duplicate witness names or keys are rejected so a configuration cannot accidentally count
    /// one signer twice. Every nonzero threshold must be a strict majority of its roster. A
    /// threshold of zero intentionally means registry-signature-only mode for low-level tooling;
    /// product trust constructors reject that mode.
    pub fn verify_with_witnesses(
        text: &str,
        registry_key: &VerifyingKey,
        witnesses: &[WitnessKey],
        threshold: usize,
    ) -> Result<Checkpoint> {
        Ok(Self::verify_witness_policy(text, registry_key, witnesses, threshold, None)?.checkpoint)
    }

    /// Verify a checkpoint and require a fresh C2SP timestamped cosignature quorum.
    ///
    /// A local HTTP receipt time is not freshness evidence: a stale mirror can replay an old
    /// operator-signed checkpoint forever. C2SP cosignatures bind a witness timestamp into each
    /// witness signature, so the caller can reject replays without changing the three-line
    /// `tlog-checkpoint` body.
    pub fn verify_with_fresh_witnesses(
        text: &str,
        registry_key: &VerifyingKey,
        witnesses: &[WitnessKey],
        threshold: usize,
        now_secs: u64,
        max_age_secs: u64,
        future_skew_secs: u64,
    ) -> Result<VerifiedCheckpoint> {
        if now_secs > i64::MAX as u64 || max_age_secs == 0 || future_skew_secs > max_age_secs {
            return Err(RegistryError::MalformedCheckpoint(
                "invalid witness freshness policy".into(),
            ));
        }
        Self::verify_witness_policy(
            text,
            registry_key,
            witnesses,
            threshold,
            Some((now_secs, max_age_secs, future_skew_secs)),
        )
    }

    fn verify_witness_policy(
        text: &str,
        registry_key: &VerifyingKey,
        witnesses: &[WitnessKey],
        threshold: usize,
        freshness: Option<(u64, u64, u64)>,
    ) -> Result<VerifiedCheckpoint> {
        if threshold != 0 && !witness_quorum_intersects(threshold, witnesses.len()) {
            return Err(RegistryError::MalformedCheckpoint(
                "witness threshold does not guarantee quorum intersection".into(),
            ));
        }
        for (index, witness) in witnesses.iter().enumerate() {
            if witnesses[..index].iter().any(|prior| {
                prior.name == witness.name
                    || bool::from(prior.key.as_bytes().ct_eq(witness.key.as_bytes()))
            }) {
                return Err(RegistryError::MalformedCheckpoint(
                    "duplicate witness configuration".into(),
                ));
            }
        }

        let checkpoint = Self::verify(text, registry_key)?;
        if threshold == 0 {
            return Ok(VerifiedCheckpoint {
                checkpoint,
                witnessed_at: None,
            });
        }
        let (body, signatures) = text
            .split_once("\n\n")
            .ok_or_else(|| RegistryError::MalformedCheckpoint("missing signature block".into()))?;
        let mut verified: Vec<u64> = witnesses
            .iter()
            .filter_map(|witness| {
                newest_valid_cosignature(body, signatures, witness.name(), witness.key(), freshness)
            })
            .collect();
        if verified.len() < threshold {
            return Err(RegistryError::MalformedCheckpoint(
                "fresh witness cosignature threshold not met".into(),
            ));
        }
        verified.sort_unstable_by(|left, right| right.cmp(left));
        Ok(VerifiedCheckpoint {
            checkpoint,
            witnessed_at: Some(verified[threshold - 1]),
        })
    }
}

fn newest_valid_cosignature(
    body: &str,
    signatures: &str,
    expected_name: &str,
    key: &VerifyingKey,
    freshness: Option<(u64, u64, u64)>,
) -> Option<u64> {
    let expected_keyhash = cosignature_key_hash(expected_name, key);
    signatures
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("— ")?;
            let (name, encoded) = rest.split_once(' ')?;
            if name != expected_name || encoded.contains(' ') {
                return None;
            }
            let blob = unb64(encoded)?;
            if blob.len() != 4 + 8 + 64 || !bool::from(blob[..4].ct_eq(expected_keyhash.as_slice()))
            {
                return None;
            }
            let timestamp = u64::from_be_bytes(blob[4..12].try_into().ok()?);
            if timestamp > i64::MAX as u64 {
                return None;
            }
            if let Some((now, max_age, future_skew)) = freshness {
                if timestamp > now.saturating_add(future_skew)
                    || timestamp < now.saturating_sub(max_age)
                {
                    return None;
                }
            }
            let signature = Signature::from_bytes(&blob[12..].try_into().ok()?);
            let message = format!("cosignature/v1\ntime {timestamp}\n{body}\n");
            key.verify_strict(message.as_bytes(), &signature)
                .is_ok()
                .then_some(timestamp)
        })
        .max()
}

fn has_valid_signature(
    body: &str,
    signatures: &str,
    expected_name: &str,
    key: &VerifyingKey,
) -> bool {
    let expected_keyhash = key_hash(expected_name, key);
    signatures.lines().any(|line| {
        let Some(rest) = line.strip_prefix("— ") else {
            return false;
        };
        let Some((name, encoded)) = rest.split_once(' ') else {
            return false;
        };
        if name != expected_name || encoded.contains(' ') {
            return false;
        }
        let Some(blob) = unb64(encoded) else {
            return false;
        };
        if blob.len() != 4 + 64 || !bool::from(blob[..4].ct_eq(expected_keyhash.as_slice())) {
            return false;
        }
        let Ok(signature_bytes) = <[u8; 64]>::try_from(&blob[4..]) else {
            return false;
        };
        let signature = Signature::from_bytes(&signature_bytes);
        // The signed bytes are the body *including* its trailing newline.
        key.verify_strict(format!("{body}\n").as_bytes(), &signature)
            .is_ok()
    })
}

/// First four bytes of `SHA-256(origin ‖ '\n' ‖ alg ‖ pubkey)`, per the C2SP note spec.
fn key_hash(origin: &str, key: &VerifyingKey) -> [u8; 4] {
    let mut hasher = Sha256::new();
    hasher.update(origin.as_bytes());
    hasher.update(b"\n");
    hasher.update([ALG_ED25519]);
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    [digest[0], digest[1], digest[2], digest[3]]
}

/// First four bytes of `SHA-256(name ‖ '\n' ‖ 0x04 ‖ pubkey)`, per
/// C2SP tlog-cosignature v1.
fn cosignature_key_hash(name: &str, key: &VerifyingKey) -> [u8; 4] {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b"\n");
    hasher.update([ALG_ED25519_COSIGNATURE]);
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    [digest[0], digest[1], digest[2], digest[3]]
}

// Minimal standard base64 with padding — one small function beats a dependency for this.
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn unb64(input: &str) -> Option<Vec<u8>> {
    if input.len() % 4 != 0
        || input
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'/' | b'='))
        || input
            .find('=')
            .is_some_and(|first| first < input.len().saturating_sub(2))
    {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for ch in input.trim().bytes() {
        if ch == b'=' {
            break;
        }
        let value = B64.iter().position(|&c| c == ch)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    (b64(&out) == input).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn checkpoint() -> Checkpoint {
        Checkpoint {
            origin: "pigeonpost.dev/registry".into(),
            size: 48_211,
            root: [7u8; 32],
        }
    }

    #[test]
    fn base64_round_trips() {
        for len in 0..40usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 13 + 5) as u8).collect();
            assert_eq!(unb64(&b64(&bytes)).unwrap(), bytes, "len {len}");
        }
    }

    #[test]
    fn a_signed_checkpoint_verifies() {
        let key = key(1);
        let signed = checkpoint().sign(&key);
        let parsed = Checkpoint::verify(&signed, &key.verifying_key()).unwrap();
        assert_eq!(parsed, checkpoint());
    }

    #[test]
    fn witness_threshold_requires_distinct_valid_signers() {
        let registry = key(1);
        let witness_a = key(2);
        let witness_b = key(3);
        let mut signed = checkpoint().sign(&registry);
        let body = checkpoint().body();
        signed.push_str(&cosignature_line("witness-a", &body, &witness_a, 100));
        signed.push_str(&cosignature_line("witness-b", &body, &witness_b, 101));
        let witnesses = vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("witness-b", witness_b.verifying_key()).unwrap(),
        ];

        assert!(Checkpoint::verify_with_witnesses(
            &signed,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_ok());
        signed = signed.replace("witness-b", "unknown-witness");
        assert!(Checkpoint::verify_with_witnesses(
            &signed,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_err());
    }

    #[test]
    fn only_strictly_intersecting_witness_quorums_are_valid() {
        assert!(witness_quorum_intersects(1, 1));
        assert!(witness_quorum_intersects(2, 3));
        assert!(!witness_quorum_intersects(0, 0));
        assert!(!witness_quorum_intersects(1, 2));
        assert!(!witness_quorum_intersects(1, 3));
        assert!(!witness_quorum_intersects(4, 3));

        let registry = key(1);
        let witness_a = key(2);
        let witness_b = key(3);
        let body = checkpoint().body();
        let mut signed = checkpoint().sign(&registry);
        signed.push_str(&cosignature_line("witness-a", &body, &witness_a, 100));
        let witnesses = vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("witness-b", witness_b.verifying_key()).unwrap(),
        ];
        assert!(Checkpoint::verify_with_witnesses(
            &signed,
            &registry.verifying_key(),
            &witnesses,
            1,
        )
        .is_err());
    }

    #[test]
    fn one_shared_roster_cannot_accept_disjoint_witness_subsets() {
        let registry = key(1);
        let witness_a = key(2);
        let witness_b = key(3);
        let witness_c = key(4);
        let witnesses = vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("witness-b", witness_b.verifying_key()).unwrap(),
            WitnessKey::new("witness-c", witness_c.verifying_key()).unwrap(),
        ];

        let fork_a = Checkpoint {
            root: [0xAA; 32],
            ..checkpoint()
        };
        let mut signed_a = fork_a.sign(&registry);
        signed_a.push_str(
            &fork_a
                .cosignature_line("witness-a", &witness_a, 100)
                .unwrap(),
        );
        signed_a.push_str(
            &fork_a
                .cosignature_line("witness-b", &witness_b, 100)
                .unwrap(),
        );

        let fork_b = Checkpoint {
            root: [0xBB; 32],
            ..checkpoint()
        };
        let mut signed_b = fork_b.sign(&registry);
        signed_b.push_str(
            &fork_b
                .cosignature_line("witness-c", &witness_c, 100)
                .unwrap(),
        );

        assert!(Checkpoint::verify_with_witnesses(
            &signed_a,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_ok());
        assert!(Checkpoint::verify_with_witnesses(
            &signed_b,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_err());
    }

    #[test]
    fn a_shared_equivocator_can_bridge_two_majority_quorums() {
        let registry = key(1);
        let witness_a = key(2);
        let equivocator = key(3);
        let witness_c = key(4);
        let witnesses = vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("equivocator", equivocator.verifying_key()).unwrap(),
            WitnessKey::new("witness-c", witness_c.verifying_key()).unwrap(),
        ];

        let fork_a = Checkpoint {
            root: [0xAA; 32],
            ..checkpoint()
        };
        let mut signed_a = fork_a.sign(&registry);
        signed_a.push_str(
            &fork_a
                .cosignature_line("witness-a", &witness_a, 100)
                .unwrap(),
        );
        signed_a.push_str(
            &fork_a
                .cosignature_line("equivocator", &equivocator, 100)
                .unwrap(),
        );

        let fork_b = Checkpoint {
            root: [0xBB; 32],
            ..checkpoint()
        };
        let mut signed_b = fork_b.sign(&registry);
        signed_b.push_str(
            &fork_b
                .cosignature_line("equivocator", &equivocator, 100)
                .unwrap(),
        );
        signed_b.push_str(
            &fork_b
                .cosignature_line("witness-c", &witness_c, 100)
                .unwrap(),
        );

        assert!(Checkpoint::verify_with_witnesses(
            &signed_a,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_ok());
        assert!(Checkpoint::verify_with_witnesses(
            &signed_b,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_ok());
    }

    #[test]
    fn duplicate_witness_configuration_never_inflates_the_threshold() {
        let registry = key(1);
        let witness = key(2);
        let body = checkpoint().body();
        let mut signed = checkpoint().sign(&registry);
        signed.push_str(&cosignature_line("witness", &body, &witness, 100));
        let witnesses = vec![
            WitnessKey::new("witness", witness.verifying_key()).unwrap(),
            WitnessKey::new("witness", witness.verifying_key()).unwrap(),
        ];
        assert!(Checkpoint::verify_with_witnesses(
            &signed,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_err());
    }

    #[test]
    fn fresh_cosignature_quorum_rejects_replay_future_and_legacy_note_signatures() {
        let registry = key(1);
        let witness_a = key(2);
        let witness_b = key(3);
        let witnesses = vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("witness-b", witness_b.verifying_key()).unwrap(),
        ];
        let body = checkpoint().body();
        let mut signed = checkpoint().sign(&registry);
        signed.push_str(&cosignature_line("witness-a", &body, &witness_a, 9_990));
        signed.push_str(&cosignature_line("witness-b", &body, &witness_b, 9_980));

        let verified = Checkpoint::verify_with_fresh_witnesses(
            &signed,
            &registry.verifying_key(),
            &witnesses,
            2,
            10_000,
            60,
            5,
        )
        .unwrap();
        assert_eq!(verified.checkpoint, checkpoint());
        assert_eq!(verified.witnessed_at, Some(9_980));

        assert!(Checkpoint::verify_with_fresh_witnesses(
            &signed,
            &registry.verifying_key(),
            &witnesses,
            2,
            10_100,
            60,
            5,
        )
        .is_err());

        let mut future = checkpoint().sign(&registry);
        future.push_str(&cosignature_line("witness-a", &body, &witness_a, 10_006));
        future.push_str(&cosignature_line("witness-b", &body, &witness_b, 10_006));
        assert!(Checkpoint::verify_with_fresh_witnesses(
            &future,
            &registry.verifying_key(),
            &witnesses,
            2,
            10_000,
            60,
            5,
        )
        .is_err());

        let mut legacy = checkpoint().sign(&registry);
        legacy.push_str(&signature_line("witness-a", &body, &witness_a));
        legacy.push_str(&signature_line("witness-b", &body, &witness_b));
        assert!(Checkpoint::verify_with_witnesses(
            &legacy,
            &registry.verifying_key(),
            &witnesses,
            2,
        )
        .is_err());
    }

    #[test]
    fn the_format_is_the_one_witnesses_parse() {
        let signed = checkpoint().sign(&key(1));
        let lines: Vec<&str> = signed.lines().collect();

        assert_eq!(lines[0], "pigeonpost.dev/registry");
        assert_eq!(lines[1], "48211");
        assert_eq!(lines[3], "", "a blank line separates body from signatures");
        assert!(lines[4].starts_with("— pigeonpost.dev/registry "));
    }

    #[test]
    fn a_checkpoint_signed_by_another_key_is_refused() {
        let signed = checkpoint().sign(&key(1));
        assert!(Checkpoint::verify(&signed, &key(2).verifying_key()).is_err());
    }

    #[test]
    fn tampering_with_the_size_breaks_the_signature() {
        // The attack: claim a smaller log to hide entries.
        let key = key(1);
        let signed = checkpoint().sign(&key).replace("48211", "48210");
        assert!(Checkpoint::verify(&signed, &key.verifying_key()).is_err());
    }

    #[test]
    fn tampering_with_the_root_breaks_the_signature() {
        let key = key(1);
        let mut tampered = checkpoint();
        tampered.root = [8u8; 32];
        let forged = format!(
            "{}\n{}",
            tampered.body(),
            signature_line_of(&checkpoint(), &key)
        );
        assert!(Checkpoint::verify(&forged, &key.verifying_key()).is_err());
    }

    #[test]
    fn malformed_input_is_refused_rather_than_panicking() {
        let key = key(1).verifying_key();
        for bad in ["", "no blank line", "a\nb\nc\n\n", "a\n\n\n— x y\n"] {
            assert!(Checkpoint::verify(bad, &key).is_err(), "{bad:?}");
        }
    }

    fn signature_line_of(cp: &Checkpoint, key: &SigningKey) -> String {
        cp.sign(key)
            .split_once("\n\n")
            .map(|(_, sigs)| sigs.to_string())
            .unwrap()
    }

    fn signature_line(name: &str, body: &str, key: &SigningKey) -> String {
        let signature = key.sign(body.as_bytes());
        let mut blob = Vec::with_capacity(68);
        blob.extend_from_slice(&key_hash(name, &key.verifying_key()));
        blob.extend_from_slice(&signature.to_bytes());
        format!("— {name} {}\n", b64(&blob))
    }

    fn cosignature_line(name: &str, body: &str, key: &SigningKey, timestamp: u64) -> String {
        assert_eq!(body, checkpoint().body());
        checkpoint().cosignature_line(name, key, timestamp).unwrap()
    }
}
