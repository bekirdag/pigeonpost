//! Signed release attestations (marketplace hardening, W1.1).
//!
//! M1 trusted an attestation statement because its SHA-256 matched the digest stored next to it.
//! That only proves the two fields agree; anyone who can write the postbox database (or sit on the
//! link) could mint both. From here every statement carries an Ed25519 signature made by the
//! postbox's attestation key, and the worker host only runs a release whose statement verifies
//! against one of the public keys in its own configuration.
//!
//! - **Signed bytes**: [`SIGNATURE_CONTEXT`] followed by the canonical statement, exactly as stored.
//!   The context keeps an attestation signature from ever being valid for anything else.
//! - **Key id**: `ppk_` and the first 16 hex digits of SHA-256 over the 32-byte public key. The
//!   statement names the key id it was signed with (`key_id`), so a signature can only verify under
//!   the key the statement itself names.
//! - **Rotation**: nestd trusts a *list* of public keys. Add the new key on every worker host
//!   first, switch the postbox to the new private key, and remove the old public key once no
//!   live release still depends on an attestation made with it (re-attest those first).
//!
//! The postbox implements the same rules (`marketplace_hardening`); the shared test vector below
//! is asserted on both sides.

use crate::util::{hex, sha256_hex};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// Domain separator of an attestation signature.
pub const SIGNATURE_CONTEXT: &str = "pigeonpost-nest-attestation-sig-v1\n";

/// The key id of a public key.
pub fn key_id(public: &VerifyingKey) -> String {
    format!("ppk_{}", &sha256_hex(public.as_bytes())[..16])
}

fn message(statement: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGNATURE_CONTEXT.len() + statement.len());
    m.extend_from_slice(SIGNATURE_CONTEXT.as_bytes());
    m.extend_from_slice(statement.as_bytes());
    m
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let t = text.trim();
    if t.len() != N * 2 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&t[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Sign a statement (the postbox does this; here for tests and tooling). Returns 128 hex digits.
pub fn sign(key: &SigningKey, statement: &str) -> String {
    hex(&key.sign(&message(statement)).to_bytes())
}

/// A signing key from its 32-byte seed in hex (the key file's content).
pub fn signing_key_from_hex(seed: &str) -> Option<SigningKey> {
    unhex::<32>(seed).map(|s| SigningKey::from_bytes(&s))
}

/// The public keys this host trusts, by key id.
#[derive(Clone, Default)]
pub struct TrustedKeys {
    keys: Vec<(String, VerifyingKey)>,
}

impl std::fmt::Debug for TrustedKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.keys.iter().map(|(id, _)| id))
            .finish()
    }
}

impl TrustedKeys {
    /// From the configured public keys (64 hex digits each). Any malformed or weak key refuses
    /// the whole list: a typo must not silently drop a key or keep a revoked one.
    pub fn from_hex(list: &[String]) -> Result<TrustedKeys, String> {
        let mut keys: Vec<(String, VerifyingKey)> = Vec::new();
        for (i, text) in list.iter().enumerate() {
            let bytes = unhex::<32>(text)
                .ok_or_else(|| format!("attestation key {i}: 64 hex digits expected"))?;
            let key = VerifyingKey::from_bytes(&bytes)
                .map_err(|_| format!("attestation key {i}: not an Ed25519 public key"))?;
            if key.is_weak() {
                return Err(format!("attestation key {i}: a weak key"));
            }
            let id = key_id(&key);
            if !keys.iter().any(|(k, _)| *k == id) {
                keys.push((id, key));
            }
        }
        Ok(TrustedKeys { keys })
    }

    pub fn from_keys(list: &[VerifyingKey]) -> TrustedKeys {
        TrustedKeys {
            keys: list.iter().map(|k| (key_id(k), *k)).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn ids(&self) -> Vec<String> {
        self.keys.iter().map(|(id, _)| id.clone()).collect()
    }

    /// Verify `signature` (128 hex digits) over `statement` under the trusted key `key_id`, and
    /// that the statement itself names that key. Strict verification: no malleable signatures.
    pub fn verify(&self, statement: &str, key_id: &str, signature: &str) -> Result<(), String> {
        let Some((_, key)) = self.keys.iter().find(|(id, _)| id == key_id) else {
            return Err("the attestation key is not trusted here".into());
        };
        let named = serde_json::from_str::<serde_json::Value>(statement)
            .ok()
            .and_then(|s| s["key_id"].as_str().map(str::to_string));
        if named.as_deref() != Some(key_id) {
            return Err("the statement does not name the key it is signed with".into());
        }
        let sig = unhex::<64>(signature).ok_or("the signature is not 128 hex digits")?;
        key.verify_strict(&message(statement), &Signature::from_bytes(&sig))
            .map_err(|_| "the attestation signature does not verify".to_string())
    }
}

#[cfg(test)]
pub(crate) fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The postbox (`marketplace_hardening`) asserts the same vector.
    #[test]
    fn the_shared_test_vector_holds() {
        let key = test_key();
        let id = key_id(&key.verifying_key());
        assert_eq!(id, "ppk_fe812c12f3ab4ce6");
        let statement = format!("{{\"key_id\":\"{id}\",\"type\":\"t\"}}");
        let sig = sign(&key, &statement);
        assert_eq!(
            &sig[..16],
            "7873323f311d7fea",
            "Ed25519 is deterministic: the same key and statement give the same signature"
        );
        let trusted = TrustedKeys::from_keys(&[key.verifying_key()]);
        trusted.verify(&statement, &id, &sig).unwrap();
    }

    #[test]
    fn only_a_trusted_key_named_by_the_statement_verifies() {
        let key = test_key();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let id = key_id(&key.verifying_key());
        let statement = format!("{{\"key_id\":\"{id}\",\"x\":1}}");
        let sig = sign(&key, &statement);
        let trusted = TrustedKeys::from_keys(&[key.verifying_key()]);
        assert!(trusted.verify(&statement, &id, &sig).is_ok());
        // Another statement, a flipped bit, an untrusted key, a statement naming another key.
        assert!(trusted
            .verify(&statement.replace("1}", "2}"), &id, &sig)
            .is_err());
        let mut bad = sig.clone().into_bytes();
        bad[0] = if bad[0] == b'0' { b'1' } else { b'0' };
        assert!(trusted
            .verify(&statement, &id, &String::from_utf8(bad).unwrap())
            .is_err());
        let oid = key_id(&other.verifying_key());
        let ostatement = format!("{{\"key_id\":\"{oid}\",\"x\":1}}");
        assert!(trusted
            .verify(&ostatement, &oid, &sign(&other, &ostatement))
            .is_err());
        let unnamed = "{\"x\":1}";
        assert!(trusted.verify(unnamed, &id, &sign(&key, unnamed)).is_err());
        // Rotation: both keys trusted, each verifies its own.
        let both = TrustedKeys::from_keys(&[key.verifying_key(), other.verifying_key()]);
        assert!(both
            .verify(&ostatement, &oid, &sign(&other, &ostatement))
            .is_ok());
        assert!(both.verify(&statement, &id, &sig).is_ok());
    }

    #[test]
    fn configured_keys_are_strict() {
        let good = hex(test_key().verifying_key().as_bytes());
        assert_eq!(
            TrustedKeys::from_hex(std::slice::from_ref(&good))
                .unwrap()
                .ids()
                .len(),
            1
        );
        assert!(TrustedKeys::from_hex(&[good.clone(), "zz".into()]).is_err());
        assert!(TrustedKeys::from_hex(&["00".repeat(32)]).is_err());
        assert!(signing_key_from_hex(&"07".repeat(32)).is_some());
        assert!(signing_key_from_hex("07").is_none());
    }
}
