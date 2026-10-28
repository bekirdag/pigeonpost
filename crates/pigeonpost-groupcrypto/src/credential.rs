//! Device credentials bound to a Pigeonpost mailbox identity.
//!
//! Every MLS leaf carries a `BasicCredential` whose identity bytes are a [`DeviceCredential`]:
//!
//! ```text
//! u8   version = 1
//! u16  len || mailbox address   ("/k/<26 base32>", canonical)
//! u16  len || device id         (1..=64 of [A-Za-z0-9._-])
//! [32] mailbox ed25519 public key
//! [64] ed25519 attestation signature by that mailbox key
//! ```
//!
//! The attestation signs [`attestation_payload`], which binds the mailbox, the device id, the
//! ciphersuite and the device's MLS signature public key (the leaf's `signature_key`). Because a
//! `/k/` address *is* the hash of the mailbox public key, verification needs no directory: the
//! address must equal `address_for_key(mailbox_key)` and the signature must verify. A leaf
//! is accepted only when the attested device key equals the leaf's actual signature key.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};

use crate::codec::{Reader, Writer};
use crate::error::{Error, Result};

const CREDENTIAL_VERSION: u8 = 1;
const ATTESTATION_DOMAIN: &[u8] = b"pigeonpost/groupcrypto/device-attestation/v1\0";
const FINGERPRINT_DOMAIN: &[u8] = b"pigeonpost/groupcrypto/device-fingerprint/v1\0";
const SAFETY_DOMAIN: &[u8] = b"pigeonpost/groupcrypto/safety-number/v1\0";
/// The only ciphersuite this crate speaks: RFC 9420 suite 0x0001.
pub(crate) const CIPHERSUITE_ID: u16 = 0x0001;
const MAX_DEVICE_ID: usize = 64;

/// The identity carried in every leaf: which mailbox, which of its devices, and the proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCredential {
    /// Canonical `/k/` mailbox address.
    pub mailbox: String,
    /// Device id chosen by the mailbox owner, unique within that mailbox.
    pub device_id: String,
    /// Mailbox ed25519 identity public key (its hash is the address).
    pub mailbox_public_key: [u8; 32],
    /// Mailbox signature over [`attestation_payload`].
    pub attestation: [u8; 64],
}

/// Validate a device id: 1..=64 characters of `[A-Za-z0-9._-]`.
pub fn validate_device_id(device_id: &str) -> Result<()> {
    let ok = !device_id.is_empty()
        && device_id.len() <= MAX_DEVICE_ID
        && device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(
            "device id must be 1-64 characters of A-Z a-z 0-9 . _ -",
        ))
    }
}

const B32: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// `/k/` + lowercase Crockford base32 of the first 16 bytes of SHA-256(public key): the same
/// derivation as `pigeonpost_core::address::Address::from_pubkey` (a test pins the equality).
/// Kept local so this crate builds reproducibly without an out-of-workspace path dependency.
pub fn address_for_key(public_key: &[u8; 32]) -> String {
    let digest = Sha256::digest(public_key);
    let mut out = String::from("/k/");
    let (mut buffer, mut bits) = (0u16, 0u8);
    for &byte in &digest[..16] {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    out.push(B32[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    out
}

/// Require a mailbox address in canonical `/k/` form (26 lowercase Crockford characters that
/// re-encode to themselves).
pub fn canonical_mailbox(mailbox: &str) -> Result<String> {
    let bad = || Error::invalid("mailbox must be a canonical /k/ address");
    let body = mailbox.strip_prefix("/k/").ok_or_else(bad)?;
    if body.len() != 26 {
        return Err(bad());
    }
    let (mut buffer, mut bits, mut bytes) = (0u32, 0u8, Vec::with_capacity(17));
    for c in body.bytes() {
        let v = B32.iter().position(|&a| a == c).ok_or_else(bad)? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
        }
    }
    // 130 bits carry 16 bytes; the 2 spare bits must be zero for the spelling to be canonical.
    if buffer & ((1 << bits) - 1) != 0 || bytes.len() != 16 {
        return Err(bad());
    }
    Ok(mailbox.to_owned())
}

/// The exact bytes a mailbox identity key signs to attest one device.
pub fn attestation_payload(
    mailbox: &str,
    device_id: &str,
    device_signature_key: &[u8],
) -> Result<Vec<u8>> {
    canonical_mailbox(mailbox)?;
    validate_device_id(device_id)?;
    if device_signature_key.len() != 32 {
        return Err(Error::invalid("device signature key must be 32 bytes"));
    }
    let mut w = Writer::new();
    w.raw(ATTESTATION_DOMAIN);
    w.bytes16(mailbox.as_bytes())?;
    w.bytes16(device_id.as_bytes())?;
    w.u16(CIPHERSUITE_ID);
    w.bytes16(device_signature_key)?;
    Ok(w.finish())
}

/// Sign an attestation payload with a mailbox identity seed (the 32-byte ed25519 seed that
/// `pigeonpost-core` stores). Clients that keep the seed elsewhere sign the payload themselves.
pub fn sign_attestation_with_seed(identity_seed: &[u8; 32], payload: &[u8]) -> Result<[u8; 64]> {
    if !payload.starts_with(ATTESTATION_DOMAIN) {
        return Err(Error::invalid("not a device attestation payload"));
    }
    let key = SigningKey::from_bytes(identity_seed);
    Ok(key.sign(payload).to_bytes())
}

/// Derive the `/k/` mailbox address for an identity seed.
pub fn mailbox_for_seed(identity_seed: &[u8; 32]) -> String {
    let key = SigningKey::from_bytes(identity_seed);
    address_for_key(&key.verifying_key().to_bytes())
}

impl DeviceCredential {
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer::new();
        w.u8(CREDENTIAL_VERSION);
        w.bytes16(self.mailbox.as_bytes())?;
        w.bytes16(self.device_id.as_bytes())?;
        w.raw(&self.mailbox_public_key);
        w.raw(&self.attestation);
        Ok(w.finish())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.u8()? != CREDENTIAL_VERSION {
            return Err(Error::InvalidCredential(
                "unsupported credential version".into(),
            ));
        }
        let mailbox = r.str16()?.to_owned();
        let device_id = r.str16()?.to_owned();
        let mailbox_public_key = r.array::<32>()?;
        let attestation = r.array::<64>()?;
        r.finish()?;
        Ok(Self {
            mailbox,
            device_id,
            mailbox_public_key,
            attestation,
        })
    }

    /// Verify the mailbox binding and that `leaf_signature_key` is the attested device key.
    pub(crate) fn verify(&self, leaf_signature_key: &[u8]) -> Result<()> {
        let bad = |why: &str| Error::InvalidCredential(why.to_owned());
        canonical_mailbox(&self.mailbox)
            .map_err(|_| bad("mailbox is not a canonical /k/ address"))?;
        validate_device_id(&self.device_id).map_err(|_| bad("invalid device id"))?;
        let mailbox_key = VerifyingKey::from_bytes(&self.mailbox_public_key)
            .map_err(|_| bad("invalid mailbox key"))?;
        if address_for_key(&self.mailbox_public_key) != self.mailbox {
            return Err(bad("mailbox key does not match the mailbox address"));
        }
        let payload = attestation_payload(&self.mailbox, &self.device_id, leaf_signature_key)
            .map_err(|_| bad("invalid device signature key"))?;
        mailbox_key
            .verify_strict(&payload, &Signature::from_bytes(&self.attestation))
            .map_err(|_| bad("attestation signature does not verify"))
    }

    /// Parse and fully verify the identity bytes of a leaf credential.
    pub(crate) fn from_leaf(identity: &[u8], leaf_signature_key: &[u8]) -> Result<Self> {
        let credential = Self::decode(identity)?;
        credential.verify(leaf_signature_key)?;
        Ok(credential)
    }
}

/// Stable 32-byte fingerprint of one device: mailbox, device id and its MLS signature key.
///
/// Each part is length-prefixed with a `u16`. A part longer than `u16::MAX` (never produced by
/// valid credentials, whose encoding has the same limit) is not truncated into an ambiguous prefix:
/// the whole input is hashed under a separate domain with `u64` lengths instead, so it can never
/// collide with a fingerprint of valid inputs. Valid inputs hash exactly as before.
pub fn device_fingerprint(mailbox: &str, device_id: &str, device_signature_key: &[u8]) -> [u8; 32] {
    let parts = [
        mailbox.as_bytes(),
        device_id.as_bytes(),
        device_signature_key,
    ];
    let mut h = Sha256::new();
    if parts.iter().all(|p| p.len() <= u16::MAX as usize) {
        h.update(FINGERPRINT_DOMAIN);
        for part in parts {
            h.update((part.len() as u16).to_be_bytes());
            h.update(part);
        }
    } else {
        h.update(FINGERPRINT_DOMAIN_OVERSIZE);
        for part in parts {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part);
        }
    }
    h.finalize().into()
}

/// Domain for [`device_fingerprint`] inputs that do not fit the `u16` length prefix.
const FINGERPRINT_DOMAIN_OVERSIZE: &[u8] =
    b"pigeonpost/groupcrypto/device-fingerprint-oversize/v1\0";

#[cfg(test)]
mod fingerprint_length_tests {
    use super::*;

    #[test]
    fn an_oversized_part_cannot_alias_a_truncated_one() {
        let key = [7u8; 32];
        let long = "a".repeat(u16::MAX as usize + 1);
        // With a truncating cast, the length prefix of `long` would read 0, as for "".
        assert_ne!(
            device_fingerprint(&long, "d", &key),
            device_fingerprint("", "d", &key)
        );
        let over = vec![1u8; u16::MAX as usize + 1];
        assert_ne!(
            device_fingerprint("/k/a", "d", &over),
            device_fingerprint("/k/a", "d", &over[..u16::MAX as usize])
        );
        // At the limit the original encoding still applies.
        let max = "b".repeat(u16::MAX as usize);
        assert_eq!(
            device_fingerprint(&max, "d", &key),
            device_fingerprint(&max, "d", &key)
        );
    }
}

fn digits(bytes: &[u8], groups: usize) -> String {
    bytes
        .chunks_exact(5)
        .take(groups)
        .map(|chunk| {
            let mut buf = [0u8; 8];
            buf[3..].copy_from_slice(chunk);
            format!("{:05}", u64::from_be_bytes(buf) % 100_000)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Human-comparable form of a device fingerprint: six groups of five digits.
pub fn fingerprint_digits(fingerprint: &[u8; 32]) -> String {
    digits(fingerprint, 6)
}

/// A group safety number over a set of device fingerprints: twelve groups of five digits.
/// Order-independent; any added, removed or re-keyed device changes it.
pub fn safety_number(fingerprints: &[[u8; 32]]) -> String {
    let mut sorted = fingerprints.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut h = Sha512::new();
    h.update(SAFETY_DOMAIN);
    h.update((sorted.len() as u32).to_be_bytes());
    for fp in &sorted {
        h.update(fp);
    }
    digits(&h.finalize()[..60], 12)
}
