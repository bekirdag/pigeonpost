//! Durable, out-of-band registry trust configuration.
//!
//! Registry responses are never allowed to populate this structure. Operators import every trust
//! anchor explicitly, and the client persists the validated result before it performs a resolve.

use std::collections::HashSet;

use pigeonpost_core::{
    keys,
    network::{is_localhost_name, is_numeric_loopback_host},
};
use pigeonpost_registry::{
    log::empty_root, witness_quorum_intersects, Checkpoint, CheckpointPin, RegistryTrust,
    WitnessKey,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{ClientError, Result};

pub const REGISTRY_TRUST_BUNDLE_VERSION: u8 = 1;
pub const REGISTRY_TRUST_RESET_CONFIRMATION: &str = "reset-registry-trust";
pub const MAX_REGISTRY_TRUST_JSON_BYTES: usize = 64 * 1024;

pub const REGISTRY_TRUST_MAX_URL_BYTES: usize = 2_048;
pub const REGISTRY_TRUST_MAX_ORIGIN_BYTES: usize = 256;
pub const REGISTRY_TRUST_MAX_WITNESSES: usize = 32;
pub const REGISTRY_TRUST_MAX_WITNESS_NAME_BYTES: usize = 128;
pub const REGISTRY_TRUST_MAX_COSIGNATURE_AGE_SECS: u64 = 24 * 60 * 60;

/// Untrusted JSON-facing representation of a registry trust bundle.
///
/// All textual keys and roots must be exactly 32 lowercase hexadecimal bytes. Unknown fields fail
/// deserialization; semantic validation happens when this is converted to
/// [`RegistryTrustBundle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryTrustInput {
    pub version: u8,
    pub registry_url: String,
    pub origin: String,
    pub checkpoint_key: String,
    pub witnesses: Vec<RegistryWitnessInput>,
    pub witness_threshold: usize,
    pub minimum_checkpoint: RegistryCheckpointInput,
    pub max_cosignature_age_seconds: u64,
    pub future_clock_skew_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryWitnessInput {
    pub name: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryCheckpointInput {
    pub size: u64,
    pub root: String,
}

/// Canonical, fully validated registry trust anchors safe to persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryTrustBundle {
    version: u8,
    registry_url: String,
    origin: String,
    checkpoint_key: String,
    witnesses: Vec<RegistryWitnessInput>,
    witness_threshold: usize,
    minimum_checkpoint: RegistryCheckpointInput,
    max_cosignature_age_seconds: u64,
    future_clock_skew_seconds: u64,
}

/// Public, secret-free view of the currently pinned registry trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryTrustStatus {
    pub bundle: RegistryTrustBundle,
    pub accepted_checkpoint: Option<RegistryCheckpointInput>,
    pub witnessed_at: Option<u64>,
    pub fresh: bool,
}

impl RegistryTrustInput {
    /// Parse a bounded strict JSON trust bundle. This is suitable for CLI file input.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_REGISTRY_TRUST_JSON_BYTES {
            return Err(ClientError::Config(
                "registry trust bundle is outside the allowed size".into(),
            ));
        }
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl TryFrom<RegistryTrustInput> for RegistryTrustBundle {
    type Error = ClientError;

    fn try_from(input: RegistryTrustInput) -> Result<Self> {
        if input.version != REGISTRY_TRUST_BUNDLE_VERSION {
            return Err(ClientError::Config(
                "unsupported registry trust bundle version".into(),
            ));
        }
        let registry_url = canonical_registry_url(&input.registry_url)?;
        validate_origin(&input.origin)?;
        if input.witnesses.is_empty()
            || input.witnesses.len() > REGISTRY_TRUST_MAX_WITNESSES
            || !witness_quorum_intersects(input.witness_threshold, input.witnesses.len())
            || input.max_cosignature_age_seconds == 0
            || input.max_cosignature_age_seconds > REGISTRY_TRUST_MAX_COSIGNATURE_AGE_SECS
            || input.future_clock_skew_seconds > input.max_cosignature_age_seconds
        {
            return Err(ClientError::Config(
                "registry witness policy is incomplete or outside the allowed bounds".into(),
            ));
        }

        let checkpoint_key = parse_public_key(&input.checkpoint_key, "registry checkpoint key")?;
        let minimum_root = parse_hex32(&input.minimum_checkpoint.root, "minimum checkpoint root")?;
        if (input.minimum_checkpoint.size == 0 && minimum_root != empty_root())
            || (input.minimum_checkpoint.size != 0 && minimum_root == empty_root())
        {
            return Err(ClientError::Config(
                "minimum checkpoint size and root do not form a canonical anchor".into(),
            ));
        }

        let mut names = HashSet::with_capacity(input.witnesses.len());
        let mut keys = HashSet::with_capacity(input.witnesses.len());
        let mut witnesses = Vec::with_capacity(input.witnesses.len());
        for witness in input.witnesses {
            if witness.name.is_empty()
                || witness.name.len() > REGISTRY_TRUST_MAX_WITNESS_NAME_BYTES
                || witness
                    .name
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
                || witness.name == input.origin
                || !names.insert(witness.name.clone())
            {
                return Err(ClientError::Config(
                    "registry witness names must be bounded, canonical, independent, and unique"
                        .into(),
                ));
            }
            let key = parse_public_key(&witness.public_key, "registry witness key")?;
            if key == checkpoint_key || !keys.insert(key) {
                return Err(ClientError::Config(
                    "registry witness keys must be unique and distinct from the checkpoint key"
                        .into(),
                ));
            }
            witnesses.push(RegistryWitnessInput {
                name: witness.name,
                public_key: lower_hex(&key),
            });
        }
        witnesses.sort_by(|left, right| left.name.cmp(&right.name));

        let bundle = Self {
            version: REGISTRY_TRUST_BUNDLE_VERSION,
            registry_url,
            origin: input.origin,
            checkpoint_key: lower_hex(&checkpoint_key),
            witnesses,
            witness_threshold: input.witness_threshold,
            minimum_checkpoint: RegistryCheckpointInput {
                size: input.minimum_checkpoint.size,
                root: lower_hex(&minimum_root),
            },
            max_cosignature_age_seconds: input.max_cosignature_age_seconds,
            future_clock_skew_seconds: input.future_clock_skew_seconds,
        };
        // Keep the registry crate as the final cryptographic-policy validator too.
        let _ = bundle.to_registry_trust()?;
        Ok(bundle)
    }
}

impl RegistryTrustBundle {
    pub fn from_registry_trust(url: &str, trust: &RegistryTrust) -> Result<Self> {
        RegistryTrustInput {
            version: REGISTRY_TRUST_BUNDLE_VERSION,
            registry_url: url.to_owned(),
            origin: trust.expected_origin().to_owned(),
            checkpoint_key: lower_hex(trust.checkpoint_key().as_bytes()),
            witnesses: trust
                .witnesses()
                .iter()
                .map(|witness| RegistryWitnessInput {
                    name: witness.name().to_owned(),
                    public_key: lower_hex(witness.key().as_bytes()),
                })
                .collect(),
            witness_threshold: trust.witness_threshold(),
            minimum_checkpoint: RegistryCheckpointInput {
                size: trust.minimum_checkpoint().size,
                root: lower_hex(&trust.minimum_checkpoint().root),
            },
            max_cosignature_age_seconds: trust.max_cosignature_age_secs(),
            future_clock_skew_seconds: trust.future_clock_skew_secs(),
        }
        .try_into()
    }

    pub fn to_registry_trust(&self) -> Result<RegistryTrust> {
        let checkpoint_key = parse_hex32(&self.checkpoint_key, "registry checkpoint key")?;
        let witnesses = self
            .witnesses
            .iter()
            .map(|witness| {
                let key = parse_public_key(&witness.public_key, "registry witness key")?;
                let key = keys::verifying_key_from_bytes(&key).map_err(|_| {
                    ClientError::Config("registry witness key is not valid Ed25519".into())
                })?;
                WitnessKey::new(witness.name.clone(), key).map_err(ClientError::from)
            })
            .collect::<Result<Vec<_>>>()?;
        RegistryTrust::new(
            self.origin.clone(),
            checkpoint_key,
            witnesses,
            self.witness_threshold,
            CheckpointPin {
                size: self.minimum_checkpoint.size,
                root: parse_hex32(&self.minimum_checkpoint.root, "minimum checkpoint root")?,
            },
            self.max_cosignature_age_seconds,
            self.future_clock_skew_seconds,
        )
        .map_err(ClientError::from)
    }

    pub fn registry_url(&self) -> &str {
        &self.registry_url
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn checkpoint_key(&self) -> &str {
        &self.checkpoint_key
    }

    pub fn witnesses(&self) -> &[RegistryWitnessInput] {
        &self.witnesses
    }

    pub const fn witness_threshold(&self) -> usize {
        self.witness_threshold
    }

    pub const fn minimum_checkpoint(&self) -> &RegistryCheckpointInput {
        &self.minimum_checkpoint
    }

    pub const fn max_cosignature_age_seconds(&self) -> u64 {
        self.max_cosignature_age_seconds
    }

    pub const fn future_clock_skew_seconds(&self) -> u64 {
        self.future_clock_skew_seconds
    }
}

impl RegistryTrustStatus {
    pub(crate) fn new(
        bundle: RegistryTrustBundle,
        checkpoint: Option<&Checkpoint>,
        witnessed_at: Option<u64>,
        now_secs: u64,
    ) -> Self {
        let accepted_checkpoint = checkpoint.map(|checkpoint| RegistryCheckpointInput {
            size: checkpoint.size,
            root: lower_hex(&checkpoint.root),
        });
        let fresh = witnessed_at.is_some_and(|timestamp| {
            timestamp <= now_secs.saturating_add(bundle.future_clock_skew_seconds)
                && timestamp >= now_secs.saturating_sub(bundle.max_cosignature_age_seconds)
        });
        Self {
            bundle,
            accepted_checkpoint,
            witnessed_at,
            fresh,
        }
    }
}

fn canonical_registry_url(input: &str) -> Result<String> {
    if input.is_empty() || input.len() > REGISTRY_TRUST_MAX_URL_BYTES {
        return Err(ClientError::Config(
            "registry URL is outside the allowed length".into(),
        ));
    }
    let mut url =
        Url::parse(input).map_err(|_| ClientError::Config("registry URL is malformed".into()))?;
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !matches!(url.path(), "" | "/")
    {
        return Err(ClientError::Config(
            "registry URL must be a pathless origin without credentials, query, or fragment".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| ClientError::Config("registry URL is malformed".into()))?;
    if is_localhost_name(host) {
        return Err(ClientError::Config(
            "registry URL cannot use a localhost DNS name".into(),
        ));
    }
    let loopback = is_numeric_loopback_host(host);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(ClientError::Config(
            "registry URL must use HTTPS except for an exact loopback test origin".into(),
        ));
    }
    url.set_path("");
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn validate_origin(origin: &str) -> Result<()> {
    if origin.is_empty()
        || origin.len() > REGISTRY_TRUST_MAX_ORIGIN_BYTES
        || origin
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(ClientError::Config(
            "registry checkpoint origin is malformed".into(),
        ));
    }
    Ok(())
}

fn parse_public_key(value: &str, field: &str) -> Result<[u8; 32]> {
    let key = parse_hex32(value, field)?;
    let verifying_key = keys::verifying_key_from_bytes(&key)
        .map_err(|_| ClientError::Config(format!("{field} is not a valid Ed25519 public key")))?;
    debug_assert!(!verifying_key.is_weak());
    Ok(key)
}

fn parse_hex32(value: &str, field: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ClientError::Config(format!(
            "{field} must be exactly 32 lowercase hexadecimal bytes"
        )));
    }
    let mut out = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair)
            .map_err(|_| ClientError::Config(format!("{field} is malformed")))?;
        out[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| ClientError::Config(format!("{field} is malformed")))?;
    }
    Ok(out)
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;

    fn input() -> RegistryTrustInput {
        let checkpoint = SigningKey::from_bytes(&[1; 32]);
        let witness = SigningKey::from_bytes(&[2; 32]);
        RegistryTrustInput {
            version: 1,
            registry_url: "https://registry.example/".into(),
            origin: "registry.example/log".into(),
            checkpoint_key: lower_hex(checkpoint.verifying_key().as_bytes()),
            witnesses: vec![RegistryWitnessInput {
                name: "witness.example/log".into(),
                public_key: lower_hex(witness.verifying_key().as_bytes()),
            }],
            witness_threshold: 1,
            minimum_checkpoint: RegistryCheckpointInput {
                size: 0,
                root: lower_hex(&empty_root()),
            },
            max_cosignature_age_seconds: 600,
            future_clock_skew_seconds: 30,
        }
    }

    #[test]
    fn bundle_is_canonical_and_round_trips_to_registry_policy() {
        let bundle = RegistryTrustBundle::try_from(input()).unwrap();
        assert_eq!(bundle.registry_url(), "https://registry.example");
        let trust = bundle.to_registry_trust().unwrap();
        assert_eq!(trust.witness_threshold(), 1);
        assert_eq!(trust.expected_origin(), "registry.example/log");
    }

    #[test]
    fn trust_bundles_require_a_strictly_intersecting_quorum() {
        assert!(RegistryTrustBundle::try_from(input()).is_ok());

        let mut two = input();
        let witness = SigningKey::from_bytes(&[3; 32]);
        two.witnesses.push(RegistryWitnessInput {
            name: "second.example/log".into(),
            public_key: lower_hex(witness.verifying_key().as_bytes()),
        });
        two.witness_threshold = 1;
        assert!(RegistryTrustBundle::try_from(two.clone()).is_err());

        let witness = SigningKey::from_bytes(&[4; 32]);
        two.witnesses.push(RegistryWitnessInput {
            name: "third.example/log".into(),
            public_key: lower_hex(witness.verifying_key().as_bytes()),
        });
        assert!(RegistryTrustBundle::try_from(two.clone()).is_err());
        two.witness_threshold = 2;
        assert!(RegistryTrustBundle::try_from(two).is_ok());
    }

    #[test]
    fn unsafe_or_ambiguous_bundles_fail_closed() {
        let mut value = serde_json::to_value(input()).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<RegistryTrustInput>(value).is_err());

        let mut candidate = input();
        candidate.witness_threshold = 0;
        assert!(RegistryTrustBundle::try_from(candidate).is_err());

        let mut candidate = input();
        candidate.registry_url = "http://registry.example".into();
        assert!(RegistryTrustBundle::try_from(candidate).is_err());

        let mut candidate = input();
        candidate.checkpoint_key.make_ascii_uppercase();
        assert!(RegistryTrustBundle::try_from(candidate).is_err());

        let mut candidate = input();
        candidate.witnesses[0].public_key = candidate.checkpoint_key.clone();
        assert!(RegistryTrustBundle::try_from(candidate).is_err());

        let mut candidate = input();
        candidate.checkpoint_key = format!("01{}", "00".repeat(31));
        assert!(RegistryTrustBundle::try_from(candidate).is_err());

        let mut candidate = input();
        candidate.minimum_checkpoint.size = 1;
        assert!(RegistryTrustBundle::try_from(candidate).is_err());
    }

    #[test]
    fn exact_loopback_http_is_test_only_escape_hatch() {
        for url in ["http://127.0.0.1:8080", "http://[::1]:8080"] {
            let mut candidate = input();
            candidate.registry_url = url.into();
            assert!(RegistryTrustBundle::try_from(candidate).is_ok(), "{url}");
        }
        for url in [
            "http://localhost:8080",
            "http://LOCALHOST:8080",
            "http://localhost.:8080",
            "https://localhost:8080",
            "https://localhost.:8080",
            "https://api.localhost:8080",
            "http://127.0.0.1.example:8080",
            "https://registry.example:0",
            "http://127.0.0.1:8080/service",
            "https://registry.example/service",
        ] {
            let mut candidate = input();
            candidate.registry_url = url.into();
            assert!(RegistryTrustBundle::try_from(candidate).is_err(), "{url}");
        }
    }
}
