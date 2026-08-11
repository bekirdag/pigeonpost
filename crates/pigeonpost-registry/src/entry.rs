//! Strict, versioned entries committed to the transparency log.
//!
//! The API representation is `{ version, seq, type, payload, ts_ms }`.  Decoding is deliberately
//! strict: unknown envelope fields, payload fields, versions, and entry kinds are errors.  The
//! private version-zero constructors exist only for an authenticated migration of the deployed
//! flat handle-entry schema; public inputs never accept that old shape.

use std::fmt;
use std::net::IpAddr;

use ed25519_dalek::{Signature, VerifyingKey};
use pigeonpost_compliance_format::{validate_compliance_epoch, ComplianceKeyId};
use pigeonpost_core::{
    keys,
    network::{is_localhost_name, is_public_network_address},
};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::handle::Handle;

/// Current registry log-entry version.
pub const LOG_ENTRY_VERSION: u8 = 1;
/// Version assigned to authenticated imports from the pre-versioned registry.
pub const LEGACY_LOG_ENTRY_VERSION: u8 = 0;

const LEGACY_LEAF_DOMAIN: &[u8] = b"pigeonpost/log-entry/v1";
const LEAF_DOMAIN: &[u8] = b"pigeonpost/log-entry/strict-v1";
const SIG_DOMAIN_CLAIM: &[u8] = b"pigeonpost/handle-claim/v1";
const SIG_DOMAIN_DIRECTORY_ENTRY_V1: &[u8] = b"pigeonpost/directory-entry/v1";
const SIG_DOMAIN_DIRECTORY_ENTRY_V2: &[u8] = b"pigeonpost/directory-entry/v2";
const SIG_DOMAIN_DIRECTORY_DRAIN_V1: &[u8] = b"pigeonpost/directory-drain/v1";
#[cfg(test)]
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
const GIB: u64 = 1024 * 1024 * 1024;

/// Version of the loft-authentication block embedded in directory log entries.
pub const DIRECTORY_AUTH_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    HandleClaim,
    HandleRotation,
    DirectoryAdd,
    DirectoryRemove,
    ComplianceKeyPublish,
}

impl EntryKind {
    fn tag(self) -> u8 {
        match self {
            Self::HandleClaim => 1,
            Self::HandleRotation => 2,
            Self::DirectoryAdd => 3,
            Self::DirectoryRemove => 4,
            Self::ComplianceKeyPublish => 5,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HandleClaim => "handle_bind",
            Self::HandleRotation => "handle_rotate",
            Self::DirectoryAdd => "directory_add",
            Self::DirectoryRemove => "directory_remove",
            Self::ComplianceKeyPublish => "compliance_key_publish",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleClaim {
    pub handle: String,
    /// Lower-case hexadecimal Ed25519 public key.
    pub pubkey: String,
    /// Opaque provider subject, namespaced as `provider:subject`.
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleRotation {
    pub handle: String,
    /// Lower-case hexadecimal Ed25519 public key.
    pub pubkey: String,
    /// Opaque provider subject, namespaced as `provider:subject`.
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryAdd {
    pub endpoint: String,
    /// Lower-case hexadecimal Ed25519 loft key.
    pub loft_pubkey: String,
    /// Empty means the loft deliberately registered without a handle.
    pub operator: String,
    pub capacity_bytes: u64,
    /// Present on every entry accepted by the public append API. `None` only decodes the
    /// never-published prototype shape so an existing leaf is not reinterpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<DirectoryAddAuthentication>,
}

/// Exact loft-signed fields that authorize a directory submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryAddAuthentication {
    pub version: u8,
    pub retention_days: u64,
    pub policy_open: bool,
    pub pow_floor: u32,
    pub max_event_bytes: u64,
    pub mutation_sequence: u64,
    /// Lower-case hexadecimal Ed25519 signature by `loft_pubkey`.
    pub loft_signature: String,
}

impl DirectoryAdd {
    #[allow(clippy::too_many_arguments)]
    pub fn authenticated(
        endpoint: String,
        loft_pubkey: String,
        operator: Option<String>,
        capacity_gb: u64,
        retention_days: u64,
        policy_open: bool,
        pow_floor: u32,
        max_event_bytes: u64,
        mutation_sequence: u64,
        loft_signature: String,
    ) -> Result<Self, EntryError> {
        let capacity_bytes = capacity_gb
            .checked_mul(GIB)
            .ok_or_else(|| EntryError("directory capacity overflows bytes".into()))?;
        Ok(Self {
            endpoint,
            loft_pubkey,
            operator: operator.unwrap_or_default(),
            capacity_bytes,
            authentication: Some(DirectoryAddAuthentication {
                version: DIRECTORY_AUTH_VERSION,
                retention_days,
                policy_open,
                pow_floor,
                max_event_bytes,
                mutation_sequence,
                loft_signature,
            }),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRemove {
    pub endpoint: String,
    /// Lower-case hexadecimal Ed25519 loft key.
    pub loft_pubkey: String,
    pub reason: String,
    /// Present on every entry accepted by the public append API. See [`DirectoryAdd::authentication`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<DirectoryRemoveAuthentication>,
}

/// Exact loft-signed fields that authorize a graceful directory removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRemoveAuthentication {
    pub version: u8,
    pub drain_after: u64,
    pub mutation_sequence: u64,
    /// Lower-case hexadecimal Ed25519 signature by `loft_pubkey`.
    pub loft_signature: String,
}

impl DirectoryRemove {
    pub fn authenticated(
        endpoint: String,
        loft_pubkey: String,
        drain_after: u64,
        mutation_sequence: u64,
        loft_signature: String,
    ) -> Self {
        Self {
            endpoint,
            loft_pubkey,
            reason: "operator_drain".into(),
            authentication: Some(DirectoryRemoveAuthentication {
                version: DIRECTORY_AUTH_VERSION,
                drain_after,
                mutation_sequence,
                loft_signature,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ComplianceKeyStatus {
    Active = 1,
    Retired = 2,
    Revoked = 3,
}

impl ComplianceKeyStatus {
    fn tag(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplianceKeyPublish {
    pub key_id: ComplianceKeyId,
    /// Lower-case hexadecimal X25519 public key.
    pub public_key: String,
    pub not_before_ms: u64,
    pub not_after_ms: u64,
    pub status: ComplianceKeyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versioned<T> {
    pub version: u8,
    pub seq: u64,
    pub payload: T,
    pub ts_ms: u64,
}

/// A closed set of entries in the registry's single append-only log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntry {
    HandleClaim(Versioned<HandleClaim>),
    HandleRotation(Versioned<HandleRotation>),
    DirectoryAdd(Versioned<DirectoryAdd>),
    DirectoryRemove(Versioned<DirectoryRemove>),
    ComplianceKeyPublish(Versioned<ComplianceKeyPublish>),
}

impl LogEntry {
    pub fn handle_claim(
        seq: u64,
        handle: String,
        pubkey: String,
        subject: String,
        ts_ms: u64,
    ) -> Self {
        Self::HandleClaim(Versioned {
            version: LOG_ENTRY_VERSION,
            seq,
            payload: HandleClaim {
                handle,
                pubkey,
                subject,
            },
            ts_ms,
        })
    }

    pub fn handle_rotation(
        seq: u64,
        handle: String,
        pubkey: String,
        subject: String,
        ts_ms: u64,
    ) -> Self {
        Self::HandleRotation(Versioned {
            version: LOG_ENTRY_VERSION,
            seq,
            payload: HandleRotation {
                handle,
                pubkey,
                subject,
            },
            ts_ms,
        })
    }

    pub fn compliance_key(seq: u64, payload: ComplianceKeyPublish, ts_ms: u64) -> Self {
        Self::ComplianceKeyPublish(Versioned {
            version: LOG_ENTRY_VERSION,
            seq,
            payload,
            ts_ms,
        })
    }

    /// Appendable directory-submission form. The nested authentication is verified again by the
    /// registry; callers cannot turn an operator-authored assertion into a public log leaf.
    pub fn directory_add(seq: u64, payload: DirectoryAdd, ts_ms: u64) -> Self {
        Self::DirectoryAdd(Versioned {
            version: LOG_ENTRY_VERSION,
            seq,
            payload,
            ts_ms,
        })
    }

    /// Appendable graceful-removal form, authorized by the loft key already bound to the endpoint.
    pub fn directory_remove(seq: u64, payload: DirectoryRemove, ts_ms: u64) -> Self {
        Self::DirectoryRemove(Versioned {
            version: LOG_ENTRY_VERSION,
            seq,
            payload,
            ts_ms,
        })
    }

    /// Construct an exact view of a pre-versioned leaf during authenticated migration.
    #[cfg(any(feature = "server", test))]
    pub(crate) fn legacy_handle(
        kind: EntryKind,
        seq: u64,
        handle: String,
        pubkey: String,
        subject: String,
        timestamp_s: u64,
    ) -> Result<Self, EntryError> {
        let ts_ms = timestamp_s
            .checked_mul(1_000)
            .ok_or_else(|| EntryError("legacy timestamp overflows milliseconds".into()))?;
        let entry = match kind {
            EntryKind::HandleClaim => Self::HandleClaim(Versioned {
                version: LEGACY_LOG_ENTRY_VERSION,
                seq,
                payload: HandleClaim {
                    handle,
                    pubkey,
                    subject,
                },
                ts_ms,
            }),
            EntryKind::HandleRotation => Self::HandleRotation(Versioned {
                version: LEGACY_LOG_ENTRY_VERSION,
                seq,
                payload: HandleRotation {
                    handle,
                    pubkey,
                    subject,
                },
                ts_ms,
            }),
            _ => return Err(EntryError("legacy rows may only be handle entries".into())),
        };
        entry.validate()?;
        Ok(entry)
    }

    pub fn kind(&self) -> EntryKind {
        match self {
            Self::HandleClaim(_) => EntryKind::HandleClaim,
            Self::HandleRotation(_) => EntryKind::HandleRotation,
            Self::DirectoryAdd(_) => EntryKind::DirectoryAdd,
            Self::DirectoryRemove(_) => EntryKind::DirectoryRemove,
            Self::ComplianceKeyPublish(_) => EntryKind::ComplianceKeyPublish,
        }
    }

    pub fn version(&self) -> u8 {
        match self {
            Self::HandleClaim(v) => v.version,
            Self::HandleRotation(v) => v.version,
            Self::DirectoryAdd(v) => v.version,
            Self::DirectoryRemove(v) => v.version,
            Self::ComplianceKeyPublish(v) => v.version,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::HandleClaim(v) => v.seq,
            Self::HandleRotation(v) => v.seq,
            Self::DirectoryAdd(v) => v.seq,
            Self::DirectoryRemove(v) => v.seq,
            Self::ComplianceKeyPublish(v) => v.seq,
        }
    }

    pub fn ts_ms(&self) -> u64 {
        match self {
            Self::HandleClaim(v) => v.ts_ms,
            Self::HandleRotation(v) => v.ts_ms,
            Self::DirectoryAdd(v) => v.ts_ms,
            Self::DirectoryRemove(v) => v.ts_ms,
            Self::ComplianceKeyPublish(v) => v.ts_ms,
        }
    }

    pub fn handle_binding(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::HandleClaim(v) => {
                Some((&v.payload.handle, &v.payload.pubkey, &v.payload.subject))
            }
            Self::HandleRotation(v) => {
                Some((&v.payload.handle, &v.payload.pubkey, &v.payload.subject))
            }
            _ => None,
        }
    }

    pub fn compliance_publication(&self) -> Option<&ComplianceKeyPublish> {
        match self {
            Self::ComplianceKeyPublish(v) => Some(&v.payload),
            _ => None,
        }
    }

    pub fn directory_addition(&self) -> Option<&DirectoryAdd> {
        match self {
            Self::DirectoryAdd(versioned) => Some(&versioned.payload),
            _ => None,
        }
    }

    pub fn directory_removal(&self) -> Option<&DirectoryRemove> {
        match self {
            Self::DirectoryRemove(versioned) => Some(&versioned.payload),
            _ => None,
        }
    }

    /// Endpoint, pinned loft key, and loft mutation sequence for authenticated directory leaves.
    pub fn authenticated_directory_mutation(&self) -> Option<(&str, &str, u64)> {
        match self {
            Self::DirectoryAdd(versioned) => {
                versioned.payload.authentication.as_ref().map(|auth| {
                    (
                        versioned.payload.endpoint.as_str(),
                        versioned.payload.loft_pubkey.as_str(),
                        auth.mutation_sequence,
                    )
                })
            }
            Self::DirectoryRemove(versioned) => {
                versioned.payload.authentication.as_ref().map(|auth| {
                    (
                        versioned.payload.endpoint.as_str(),
                        versioned.payload.loft_pubkey.as_str(),
                        auth.mutation_sequence,
                    )
                })
            }
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), EntryError> {
        let version = self.version();
        if version != LOG_ENTRY_VERSION
            && !(version == LEGACY_LOG_ENTRY_VERSION
                && matches!(self, Self::HandleClaim(_) | Self::HandleRotation(_)))
        {
            return Err(EntryError(format!(
                "unsupported log entry version {version}"
            )));
        }

        match self {
            Self::HandleClaim(v) => validate_handle(
                &v.payload.handle,
                &v.payload.pubkey,
                &v.payload.subject,
                v.version,
                v.ts_ms,
            ),
            Self::HandleRotation(v) => validate_handle(
                &v.payload.handle,
                &v.payload.pubkey,
                &v.payload.subject,
                v.version,
                v.ts_ms,
            ),
            Self::DirectoryAdd(v) => validate_directory_add(v),
            Self::DirectoryRemove(v) => validate_directory_remove(v),
            Self::ComplianceKeyPublish(v) => validate_compliance(v),
        }
    }

    /// Exact bytes committed as an RFC 6962 leaf.
    pub fn leaf_bytes(&self) -> Result<Vec<u8>, EntryError> {
        self.validate()?;

        if self.version() == LEGACY_LOG_ENTRY_VERSION {
            return self.legacy_leaf_bytes();
        }

        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(LEAF_DOMAIN);
        out.push(LOG_ENTRY_VERSION);
        out.push(self.kind().tag());
        out.extend_from_slice(&self.seq().to_le_bytes());

        match self {
            Self::HandleClaim(v) => {
                push_handle(
                    &mut out,
                    &v.payload.handle,
                    &v.payload.pubkey,
                    &v.payload.subject,
                )?;
            }
            Self::HandleRotation(v) => {
                push_handle(
                    &mut out,
                    &v.payload.handle,
                    &v.payload.pubkey,
                    &v.payload.subject,
                )?;
            }
            Self::DirectoryAdd(v) => {
                push_field(&mut out, v.payload.endpoint.as_bytes())?;
                push_field(&mut out, v.payload.loft_pubkey.as_bytes())?;
                push_field(&mut out, v.payload.operator.as_bytes())?;
                out.extend_from_slice(&v.payload.capacity_bytes.to_le_bytes());
                if let Some(auth) = &v.payload.authentication {
                    out.push(auth.version);
                    out.extend_from_slice(&auth.retention_days.to_le_bytes());
                    out.push(u8::from(auth.policy_open));
                    out.extend_from_slice(&auth.pow_floor.to_le_bytes());
                    out.extend_from_slice(&auth.max_event_bytes.to_le_bytes());
                    out.extend_from_slice(&auth.mutation_sequence.to_le_bytes());
                    out.extend_from_slice(&decode_hex64(&auth.loft_signature).ok_or_else(
                        || EntryError("loft_signature must be 64 lower-case hex bytes".into()),
                    )?);
                }
            }
            Self::DirectoryRemove(v) => {
                push_field(&mut out, v.payload.endpoint.as_bytes())?;
                push_field(&mut out, v.payload.loft_pubkey.as_bytes())?;
                push_field(&mut out, v.payload.reason.as_bytes())?;
                if let Some(auth) = &v.payload.authentication {
                    out.push(auth.version);
                    out.extend_from_slice(&auth.drain_after.to_le_bytes());
                    out.extend_from_slice(&auth.mutation_sequence.to_le_bytes());
                    out.extend_from_slice(&decode_hex64(&auth.loft_signature).ok_or_else(
                        || EntryError("loft_signature must be 64 lower-case hex bytes".into()),
                    )?);
                }
            }
            Self::ComplianceKeyPublish(v) => {
                out.extend_from_slice(
                    &v.payload
                        .key_id
                        .encode()
                        .map_err(|e| EntryError(e.to_string()))?,
                );
                out.extend_from_slice(&decode_hex32(&v.payload.public_key).ok_or_else(|| {
                    EntryError("public_key must be 32 lower-case hex bytes".into())
                })?);
                out.extend_from_slice(&v.payload.not_before_ms.to_le_bytes());
                out.extend_from_slice(&v.payload.not_after_ms.to_le_bytes());
                out.push(v.payload.status.tag());
            }
        }
        out.extend_from_slice(&self.ts_ms().to_le_bytes());
        Ok(out)
    }

    fn legacy_leaf_bytes(&self) -> Result<Vec<u8>, EntryError> {
        let (handle, pubkey, subject) = self
            .handle_binding()
            .ok_or_else(|| EntryError("only handle entries have a legacy codec".into()))?;
        if self.ts_ms() % 1_000 != 0 {
            return Err(EntryError(
                "legacy timestamp is not an exact number of seconds".into(),
            ));
        }
        let mut out = Vec::with_capacity(160);
        out.extend_from_slice(LEGACY_LEAF_DOMAIN);
        out.push(self.kind().tag());
        out.extend_from_slice(&self.seq().to_le_bytes());
        push_handle(&mut out, handle, pubkey, subject)?;
        out.extend_from_slice(&(self.ts_ms() / 1_000).to_le_bytes());
        Ok(out)
    }
}

fn validate_current(version: u8) -> Result<(), EntryError> {
    if version == LOG_ENTRY_VERSION {
        Ok(())
    } else {
        Err(EntryError(format!(
            "unsupported log entry version {version}"
        )))
    }
}

fn validate_handle(
    handle: &str,
    pubkey: &str,
    subject: &str,
    version: u8,
    ts_ms: u64,
) -> Result<(), EntryError> {
    if version != LOG_ENTRY_VERSION && version != LEGACY_LOG_ENTRY_VERSION {
        return Err(EntryError(format!(
            "unsupported log entry version {version}"
        )));
    }
    if version == LEGACY_LOG_ENTRY_VERSION && ts_ms % 1_000 != 0 {
        return Err(EntryError(
            "legacy timestamp is not an exact number of seconds".into(),
        ));
    }
    if !handle.starts_with('/')
        || handle.len() > 256
        || handle.bytes().any(|b| b.is_ascii_control())
    {
        return Err(EntryError("handle is not canonical".into()));
    }
    let current_handle = if version == LOG_ENTRY_VERSION {
        let parsed = Handle::parse(handle)
            .map_err(|_| EntryError("handle is not a canonical current provider handle".into()))?;
        if parsed.as_path() != handle {
            return Err(EntryError("handle is not canonical".into()));
        }
        Some(parsed)
    } else {
        // Version zero is an immutable view of the released pre-1.0 log. In particular, its
        // historical `/gh` bytes remain decodable for checkpoint verification and audit, but this
        // branch is never used by a public claim or resolution parser.
        None
    };
    validate_hex32("pubkey", pubkey)?;
    let Some((namespace, opaque)) = subject.split_once(':') else {
        return Err(EntryError("subject must be provider-namespaced".into()));
    };
    if namespace.is_empty()
        || opaque.is_empty()
        || subject.len() > 512
        || subject.bytes().any(|b| b.is_ascii_control())
    {
        return Err(EntryError("subject is malformed".into()));
    }
    if current_handle
        .as_ref()
        .is_some_and(|handle| handle.namespace() != namespace)
    {
        return Err(EntryError(
            "subject namespace does not match handle namespace".into(),
        ));
    }
    Ok(())
}

fn validate_compliance(v: &Versioned<ComplianceKeyPublish>) -> Result<(), EntryError> {
    validate_current(v.version)?;
    v.payload
        .key_id
        .validate()
        .map_err(|e| EntryError(e.to_string()))?;
    validate_hex32("public_key", &v.payload.public_key)?;
    if v.payload.key_id.authority == [0u8; 32] {
        return Err(EntryError(
            "compliance authority must not be all zeroes".into(),
        ));
    }
    if v.payload.public_key == "00".repeat(32) {
        return Err(EntryError(
            "compliance public key must not be all zeroes".into(),
        ));
    }
    validate_compliance_epoch(
        &v.payload.key_id,
        v.payload.not_before_ms,
        v.payload.not_after_ms,
    )
    .map_err(|error| EntryError(error.to_string()))
}

fn validate_directory_add(v: &Versioned<DirectoryAdd>) -> Result<(), EntryError> {
    validate_current(v.version)?;
    validate_endpoint(&v.payload.endpoint)?;
    let key = validate_ed25519_key("loft_pubkey", &v.payload.loft_pubkey)?;
    if v.payload.capacity_bytes == 0 {
        return Err(EntryError("capacity_bytes must be non-zero".into()));
    }

    let Some(auth) = &v.payload.authentication else {
        // Read compatibility for the prototype codec. The registry append path separately refuses
        // this shape, because it does not carry independently verifiable loft authorization.
        if v.payload.operator.is_empty() || v.payload.operator.len() > 256 {
            return Err(EntryError("operator must be 1..=256 bytes".into()));
        }
        return Ok(());
    };
    if auth.version != DIRECTORY_AUTH_VERSION {
        return Err(EntryError(
            "unsupported directory authentication version".into(),
        ));
    }
    if v.payload.operator.len() > 256
        || v.payload
            .operator
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(EntryError("operator must be at most 256 safe bytes".into()));
    }
    if v.payload.capacity_bytes % GIB != 0 {
        return Err(EntryError(
            "authenticated capacity_bytes must be whole GiB".into(),
        ));
    }
    let capacity_gb = v.payload.capacity_bytes / GIB;
    if auth.retention_days == 0
        || auth.max_event_bytes == 0
        || auth.max_event_bytes > 2 * 1024 * 1024
    {
        return Err(EntryError(
            "directory retention and event-size claims are invalid".into(),
        ));
    }
    let signature = decode_hex64(&auth.loft_signature)
        .ok_or_else(|| EntryError("loft_signature must be 64 lower-case hex bytes".into()))?;
    let payload = directory_add_claim_payload(
        &v.payload.endpoint,
        &v.payload.loft_pubkey,
        (!v.payload.operator.is_empty()).then_some(v.payload.operator.as_str()),
        capacity_gb,
        auth.retention_days,
        auth.policy_open,
        auth.pow_floor,
        auth.max_event_bytes,
        auth.mutation_sequence,
    )?;
    keys::verify(&key, &payload, &Signature::from_bytes(&signature))
        .map_err(|_| EntryError("directory addition is not signed by the loft key".into()))
}

fn validate_directory_remove(v: &Versioned<DirectoryRemove>) -> Result<(), EntryError> {
    validate_current(v.version)?;
    validate_endpoint(&v.payload.endpoint)?;
    let key = validate_ed25519_key("loft_pubkey", &v.payload.loft_pubkey)?;

    let Some(auth) = &v.payload.authentication else {
        if v.payload.reason.is_empty() || v.payload.reason.len() > 512 {
            return Err(EntryError("reason must be 1..=512 bytes".into()));
        }
        return Ok(());
    };
    if auth.version != DIRECTORY_AUTH_VERSION || v.payload.reason != "operator_drain" {
        return Err(EntryError(
            "authenticated removals require the operator_drain reason and supported auth version"
                .into(),
        ));
    }
    let signature = decode_hex64(&auth.loft_signature)
        .ok_or_else(|| EntryError("loft_signature must be 64 lower-case hex bytes".into()))?;
    let payload = directory_remove_claim_payload(
        &v.payload.endpoint,
        auth.drain_after,
        auth.mutation_sequence,
    )?;
    keys::verify(&key, &payload, &Signature::from_bytes(&signature))
        .map_err(|_| EntryError("directory removal is not signed by the loft key".into()))
}

fn validate_ed25519_key(field: &str, value: &str) -> Result<VerifyingKey, EntryError> {
    let bytes = decode_hex32(value)
        .ok_or_else(|| EntryError(format!("{field} must be 32 lower-case hex bytes")))?;
    keys::verifying_key_from_bytes(&bytes).map_err(|_| EntryError(format!("{field} is invalid")))
}

/// Canonical loft-signed directory submission bytes. Sequence zero preserves the legacy v1
/// authorization; every positive sequence uses the replay-resistant v2 domain.
#[allow(clippy::too_many_arguments)]
pub fn directory_add_claim_payload(
    endpoint: &str,
    loft_pubkey: &str,
    operator: Option<&str>,
    capacity_gb: u64,
    retention_days: u64,
    policy_open: bool,
    pow_floor: u32,
    max_event_bytes: u64,
    mutation_sequence: u64,
) -> Result<Vec<u8>, EntryError> {
    let domain = if mutation_sequence == 0 {
        SIG_DOMAIN_DIRECTORY_ENTRY_V1
    } else {
        SIG_DOMAIN_DIRECTORY_ENTRY_V2
    };
    let mut out = Vec::with_capacity(domain.len() + endpoint.len() + 160);
    out.extend_from_slice(domain);
    push_field(&mut out, endpoint.as_bytes())?;
    push_field(&mut out, loft_pubkey.as_bytes())?;
    push_field(&mut out, operator.unwrap_or("").as_bytes())?;
    out.extend_from_slice(&capacity_gb.to_le_bytes());
    out.extend_from_slice(&retention_days.to_le_bytes());
    out.push(u8::from(policy_open));
    out.extend_from_slice(&pow_floor.to_le_bytes());
    out.extend_from_slice(&max_event_bytes.to_le_bytes());
    if mutation_sequence != 0 {
        out.extend_from_slice(&mutation_sequence.to_le_bytes());
    }
    Ok(out)
}

/// Canonical loft-signed graceful-removal bytes.
pub fn directory_remove_claim_payload(
    endpoint: &str,
    drain_after: u64,
    mutation_sequence: u64,
) -> Result<Vec<u8>, EntryError> {
    let mut out = Vec::with_capacity(SIG_DOMAIN_DIRECTORY_DRAIN_V1.len() + endpoint.len() + 20);
    out.extend_from_slice(SIG_DOMAIN_DIRECTORY_DRAIN_V1);
    push_field(&mut out, endpoint.as_bytes())?;
    out.extend_from_slice(&drain_after.to_le_bytes());
    out.extend_from_slice(&mutation_sequence.to_le_bytes());
    Ok(out)
}

fn validate_endpoint(endpoint: &str) -> Result<(), EntryError> {
    const MESSAGE: &str =
        "directory endpoint must be a public HTTPS origin or an exact numeric loopback HTTP origin";
    let parsed = url::Url::parse(endpoint).map_err(|_| EntryError(MESSAGE.into()))?;
    let allowed_origin = match (parsed.scheme(), parsed.host()) {
        ("http", Some(url::Host::Ipv4(address))) => address.is_loopback(),
        ("http", Some(url::Host::Ipv6(address))) => address.is_loopback(),
        ("https", Some(url::Host::Ipv4(address))) => is_public_network_address(IpAddr::V4(address)),
        ("https", Some(url::Host::Ipv6(address))) => is_public_network_address(IpAddr::V6(address)),
        ("https", Some(url::Host::Domain(domain))) => !is_localhost_name(domain),
        _ => false,
    };
    if endpoint.len() > 2_048
        || !allowed_origin
        || parsed.cannot_be_a_base()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port() == Some(0)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(EntryError(MESSAGE.into()));
    }
    Ok(())
}

fn validate_hex32(field: &str, value: &str) -> Result<(), EntryError> {
    if decode_hex32(value).is_none() {
        return Err(EntryError(format!(
            "{field} must be 32 lower-case hex bytes"
        )));
    }
    Ok(())
}

fn decode_hex32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

fn decode_hex64(value: &str) -> Option<[u8; 64]> {
    if value.len() != 128
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut out = [0u8; 64];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

fn push_handle(
    out: &mut Vec<u8>,
    handle: &str,
    pubkey: &str,
    subject: &str,
) -> Result<(), EntryError> {
    push_field(out, handle.as_bytes())?;
    push_field(out, pubkey.as_bytes())?;
    push_field(out, subject.as_bytes())
}

/// Length prefixes make field boundaries unambiguous.
fn push_field(out: &mut Vec<u8>, field: &[u8]) -> Result<(), EntryError> {
    let len = u32::try_from(field.len()).map_err(|_| EntryError("field is too large".into()))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(field);
    Ok(())
}

/// What a claimant signs with the key being bound.
pub fn claim_payload(handle: &str, pubkey: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SIG_DOMAIN_CLAIM.len() + handle.len() + 40);
    out.extend_from_slice(SIG_DOMAIN_CLAIM);
    // Handles are bounded far below u32::MAX.
    push_field(&mut out, handle.as_bytes()).expect("a validated handle is bounded");
    out.extend_from_slice(pubkey);
    out
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntry {
    version: u8,
    seq: u64,
    #[serde(rename = "type")]
    entry_type: String,
    payload: serde_json::Value,
    ts_ms: u64,
}

#[derive(Serialize)]
struct WireEntryRef<'a, T> {
    version: u8,
    seq: u64,
    #[serde(rename = "type")]
    entry_type: &'static str,
    payload: &'a T,
    ts_ms: u64,
}

impl Serialize for LogEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::HandleClaim(v) => WireEntryRef {
                version: v.version,
                seq: v.seq,
                entry_type: EntryKind::HandleClaim.as_str(),
                payload: &v.payload,
                ts_ms: v.ts_ms,
            }
            .serialize(serializer),
            Self::HandleRotation(v) => WireEntryRef {
                version: v.version,
                seq: v.seq,
                entry_type: EntryKind::HandleRotation.as_str(),
                payload: &v.payload,
                ts_ms: v.ts_ms,
            }
            .serialize(serializer),
            Self::DirectoryAdd(v) => WireEntryRef {
                version: v.version,
                seq: v.seq,
                entry_type: EntryKind::DirectoryAdd.as_str(),
                payload: &v.payload,
                ts_ms: v.ts_ms,
            }
            .serialize(serializer),
            Self::DirectoryRemove(v) => WireEntryRef {
                version: v.version,
                seq: v.seq,
                entry_type: EntryKind::DirectoryRemove.as_str(),
                payload: &v.payload,
                ts_ms: v.ts_ms,
            }
            .serialize(serializer),
            Self::ComplianceKeyPublish(v) => WireEntryRef {
                version: v.version,
                seq: v.seq,
                entry_type: EntryKind::ComplianceKeyPublish.as_str(),
                payload: &v.payload,
                ts_ms: v.ts_ms,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for LogEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireEntry::deserialize(deserializer)?;
        let entry = match wire.entry_type.as_str() {
            "handle_bind" => Self::HandleClaim(Versioned {
                version: wire.version,
                seq: wire.seq,
                payload: serde_json::from_value(wire.payload).map_err(D::Error::custom)?,
                ts_ms: wire.ts_ms,
            }),
            "handle_rotate" => Self::HandleRotation(Versioned {
                version: wire.version,
                seq: wire.seq,
                payload: serde_json::from_value(wire.payload).map_err(D::Error::custom)?,
                ts_ms: wire.ts_ms,
            }),
            "directory_add" => Self::DirectoryAdd(Versioned {
                version: wire.version,
                seq: wire.seq,
                payload: serde_json::from_value(wire.payload).map_err(D::Error::custom)?,
                ts_ms: wire.ts_ms,
            }),
            "directory_remove" => Self::DirectoryRemove(Versioned {
                version: wire.version,
                seq: wire.seq,
                payload: serde_json::from_value(wire.payload).map_err(D::Error::custom)?,
                ts_ms: wire.ts_ms,
            }),
            "compliance_key_publish" => Self::ComplianceKeyPublish(Versioned {
                version: wire.version,
                seq: wire.seq,
                payload: serde_json::from_value(wire.payload).map_err(D::Error::custom)?,
                ts_ms: wire.ts_ms,
            }),
            other => return Err(D::Error::custom(format!("unknown log entry type {other}"))),
        };
        entry.validate().map_err(D::Error::custom)?;
        Ok(entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryError(pub String);

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EntryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use pigeonpost_compliance_format::{CompliancePurpose, Jurisdiction};

    fn claim(seq: u64) -> LogEntry {
        LogEntry::handle_claim(
            seq,
            "/github/alice".into(),
            "11".repeat(32),
            "github:alice".into(),
            1_786_105_721_000,
        )
    }

    #[test]
    fn strict_wire_round_trips() {
        let entry = claim(7);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"type\":\"handle_bind\""));
        assert_eq!(serde_json::from_str::<LogEntry>(&json).unwrap(), entry);
    }

    #[test]
    fn unknown_envelope_fields_kinds_versions_and_payload_fields_fail_closed() {
        let base = serde_json::to_value(claim(0)).unwrap();
        for mutate in [
            |v: &mut serde_json::Value| v["extra"] = serde_json::json!(true),
            |v: &mut serde_json::Value| v["type"] = serde_json::json!("future_kind"),
            |v: &mut serde_json::Value| v["version"] = serde_json::json!(99),
            |v: &mut serde_json::Value| v["payload"]["extra"] = serde_json::json!(true),
        ] {
            let mut value = base.clone();
            mutate(&mut value);
            assert!(serde_json::from_value::<LogEntry>(value).is_err());
        }
    }

    #[test]
    fn old_flat_shape_is_not_an_api_compatibility_backdoor() {
        // This is the exact pre-1.0 API shape, including its former `/gh` namespace.
        let old = serde_json::json!({
            "index": 0,
            "kind": "handle_bind",
            "handle": "/gh/alice",
            "pubkey": "11".repeat(32),
            "subject": "gh:alice",
            "timestamp": 1_786_105_721u64
        });
        assert!(serde_json::from_value::<LogEntry>(old).is_err());
    }

    #[test]
    fn current_entries_reject_legacy_or_mismatched_provider_namespaces() {
        let legacy_alias = LogEntry::handle_claim(
            0,
            "/gh/alice".into(),
            "11".repeat(32),
            "gh:alice".into(),
            1_786_105_721_000,
        );
        assert!(legacy_alias.validate().is_err());

        let mismatched = LogEntry::handle_claim(
            0,
            "/github/alice".into(),
            "11".repeat(32),
            "gh:alice".into(),
            1_786_105_721_000,
        );
        assert!(mismatched.validate().is_err());
    }

    #[test]
    fn directory_endpoints_are_strict_public_or_numeric_loopback_origins() {
        for accepted in [
            "https://loft.example",
            "https://8.8.8.8",
            "https://[2606:4700:4700::1111]",
            "http://127.0.0.1:7717",
            "http://[::1]:7717",
        ] {
            assert!(validate_endpoint(accepted).is_ok(), "rejected {accepted}");
        }

        for rejected in [
            "ws://loft.example",
            "wss://loft.example",
            "http://loft.example",
            "http://localhost:7717",
            "http://8.8.8.8",
            "https://127.0.0.1",
            "https://10.0.0.1",
            "https://localhost",
            "https://localhost.",
            "https://api.localhost",
            "https://API.LOCALHOST.",
            "https://loft.example:0",
            "https://loft.example/path",
            "https://loft.example?query=yes",
            "https://loft.example/#fragment",
            "https://user@loft.example",
            "https://user:secret@loft.example",
        ] {
            assert!(validate_endpoint(rejected).is_err(), "accepted {rejected}");
        }
    }

    #[test]
    fn authenticated_legacy_view_preserves_the_exact_old_leaf() {
        let legacy = LogEntry::legacy_handle(
            EntryKind::HandleClaim,
            0,
            "/gh/alice".into(),
            "11".repeat(32),
            "gh:alice".into(),
            1_786_105_721,
        )
        .unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(LEGACY_LEAF_DOMAIN);
        expected.push(1);
        expected.extend_from_slice(&0u64.to_le_bytes());
        push_handle(&mut expected, "/gh/alice", &"11".repeat(32), "gh:alice").unwrap();
        expected.extend_from_slice(&1_786_105_721u64.to_le_bytes());
        assert_eq!(legacy.leaf_bytes().unwrap(), expected);
    }

    #[test]
    fn compliance_key_ids_and_validity_are_committed_and_validated() {
        const FEBRUARY_2024: u64 = 1_706_745_600_000;
        const MARCH_2024: u64 = 1_709_251_200_000;
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Eu,
            [7; 32],
            FEBRUARY_2024,
            1,
        );
        let entry = LogEntry::compliance_key(
            4,
            ComplianceKeyPublish {
                key_id,
                public_key: "22".repeat(32),
                not_before_ms: key_id.epoch_start_ms,
                not_after_ms: MARCH_2024,
                status: ComplianceKeyStatus::Active,
            },
            key_id.epoch_start_ms,
        );
        assert!(entry.validate().is_ok());

        let mut fixed_31_days = entry.clone();
        if let LogEntry::ComplianceKeyPublish(v) = &mut fixed_31_days {
            v.payload.not_after_ms = v.payload.not_before_ms + 31 * DAY_MS;
        }
        assert!(fixed_31_days.validate().is_err());
        assert_ne!(entry.leaf_bytes().unwrap(), claim(4).leaf_bytes().unwrap());
    }

    #[test]
    fn trace_key_publications_require_one_exact_aligned_utc_day() {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [7; 32],
            2 * DAY_MS,
            1,
        );
        let entry = LogEntry::compliance_key(
            0,
            ComplianceKeyPublish {
                key_id,
                public_key: "22".repeat(32),
                not_before_ms: key_id.epoch_start_ms,
                not_after_ms: key_id.epoch_start_ms + DAY_MS,
                status: ComplianceKeyStatus::Active,
            },
            key_id.epoch_start_ms,
        );
        assert!(entry.validate().is_ok());

        for duration in [DAY_MS - 1, DAY_MS + 1, 2 * DAY_MS] {
            let mut invalid = entry.clone();
            if let LogEntry::ComplianceKeyPublish(value) = &mut invalid {
                value.payload.not_after_ms = value.payload.not_before_ms + duration;
            }
            assert!(invalid.validate().is_err());
        }

        let unaligned_id = ComplianceKeyId::new(
            CompliancePurpose::IdentityTrace,
            Jurisdiction::Test,
            [8; 32],
            2 * DAY_MS + 1,
            1,
        );
        let unaligned = LogEntry::compliance_key(
            1,
            ComplianceKeyPublish {
                key_id: unaligned_id,
                public_key: "33".repeat(32),
                not_before_ms: unaligned_id.epoch_start_ms,
                not_after_ms: unaligned_id.epoch_start_ms + DAY_MS,
                status: ComplianceKeyStatus::Active,
            },
            unaligned_id.epoch_start_ms,
        );
        assert!(unaligned.validate().is_err());
    }

    #[test]
    fn field_boundaries_and_variant_tags_are_unambiguous() {
        let a = LogEntry::handle_claim(
            0,
            "/github/ab".into(),
            "11".repeat(32),
            "github:c".into(),
            1,
        );
        let b = LogEntry::handle_claim(
            0,
            "/github/a".into(),
            "11".repeat(32),
            "github:bc".into(),
            1,
        );
        let rotation = LogEntry::handle_rotation(
            0,
            "/github/ab".into(),
            "11".repeat(32),
            "github:c".into(),
            1,
        );
        assert_ne!(a.leaf_bytes().unwrap(), b.leaf_bytes().unwrap());
        assert_ne!(a.leaf_bytes().unwrap(), rotation.leaf_bytes().unwrap());
    }
}
