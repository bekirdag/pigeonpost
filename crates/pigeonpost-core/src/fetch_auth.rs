//! Fetch authentication.
//!
//! Without this, anyone could bulk-download every ciphertext addressed to an agent — exactly the
//! harvest-now-decrypt-later trap `docs/product.md` rules out. Mail is encrypted, but a pile of
//! someone's ciphertext plus one future key compromise is the whole history.
//!
//! Stateless by design: the proof is a signature over
//! `(loft_pubkey, canonical_loft_origin, minute, cursor)`, valid within a few minutes either side.
//! No challenge round-trip, no server-side nonce table — a loft stays dumb storage, and an agent
//! that wakes, drains, and disconnects pays one signature.
//!
//! Binding both the loft key and its canonical origin stops a hostile endpoint from claiming a
//! different loft's public key, collecting a valid proof, and replaying that credential to the
//! honest loft.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::error::{Error, Result};
use crate::keys::{self, Identity};
use crate::network::{is_localhost_name, is_numeric_loopback_host};

const SIG_DOMAIN_FETCH: &[u8] = b"pigeonpost/fetch-auth/v2";

/// Same bound as the loft transport's origin validator. The signed field is public, but it is
/// attacker-influenced at manual configuration boundaries and must never drive an unbounded
/// allocation.
pub const MAX_LOFT_ORIGIN_BYTES: usize = 2_048;

/// How far either side of the loft's clock a proof stays valid. Wide enough for ordinary clock
/// drift on a laptop that has been asleep, narrow enough that a captured proof is worthless
/// within minutes.
pub const CLOCK_SKEW_MINUTES: u64 = 5;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FetchAuth {
    /// Whose mailbox is being drained. Must be the signer.
    pub recipient: [u8; 32],
    /// Which loft this proof is for.
    pub loft_pubkey: [u8; 32],
    /// Exact canonical service origin this credential may be presented to.
    pub loft_origin: String,
    /// Unix time in whole minutes.
    pub minute: u64,
    /// Deliver everything stored after this point.
    pub cursor: u64,
    #[serde(with = "BigArray")]
    pub signature: [u8; 64],
}

impl core::fmt::Debug for FetchAuth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The signature, mailbox, cursor, and origin together are a replayable short-lived fetch
        // credential. Withhold the whole proof instead of relying on every caller to remember it.
        f.write_str("FetchAuth(redacted)")
    }
}

impl FetchAuth {
    pub fn new(
        identity: &Identity,
        loft_pubkey: &[u8; 32],
        loft_origin: &str,
        minute: u64,
        cursor: u64,
    ) -> Result<Self> {
        validate_loft_origin(loft_origin)?;
        let recipient = identity.verifying_key().to_bytes();
        let payload = payload(&recipient, loft_pubkey, loft_origin, minute, cursor);
        Ok(FetchAuth {
            recipient,
            loft_pubkey: *loft_pubkey,
            loft_origin: loft_origin.to_owned(),
            minute,
            cursor,
            signature: identity.sign(&payload).to_bytes(),
        })
    }

    /// Verify against this loft's key and clock. Returns the authenticated mailbox owner.
    pub fn verify(
        &self,
        loft_pubkey: &[u8; 32],
        loft_origin: &str,
        now_minute: u64,
    ) -> Result<VerifyingKey> {
        // Reported as a bad signature rather than its own variant: a caller learns "no", never
        // which check said so. A proof bound to another loft is an invalid credential here, not a
        // malformed request.
        validate_loft_origin(loft_origin)?;
        validate_loft_origin(&self.loft_origin)?;
        if self.loft_pubkey != *loft_pubkey || self.loft_origin != loft_origin {
            return Err(Error::BadSignature);
        }
        if self.minute.abs_diff(now_minute) > CLOCK_SKEW_MINUTES {
            return Err(Error::StaleTimestamp);
        }

        let key = keys::verifying_key_from_bytes(&self.recipient)?;
        let payload = payload(
            &self.recipient,
            &self.loft_pubkey,
            &self.loft_origin,
            self.minute,
            self.cursor,
        );
        keys::verify(&key, &payload, &Signature::from_bytes(&self.signature))?;
        Ok(key)
    }
}

/// Validate the exact origin representation covered by fetch credentials.
///
/// HTTPS is required on the network. Plain HTTP is reserved for exact numeric loopback origins,
/// keeping local development possible without creating a production downgrade. The input must
/// already be canonical so two components cannot sign and compare different spellings.
pub fn validate_loft_origin(origin: &str) -> Result<()> {
    if origin.is_empty() || origin.len() > MAX_LOFT_ORIGIN_BYTES {
        return Err(Error::MalformedEnvelope("invalid loft origin"));
    }
    let parsed =
        url::Url::parse(origin).map_err(|_| Error::MalformedEnvelope("invalid loft origin"))?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.host_str().is_none()
        || parsed.port() == Some(0)
    {
        return Err(Error::MalformedEnvelope("invalid loft origin"));
    }
    let host = parsed
        .host_str()
        .ok_or(Error::MalformedEnvelope("invalid loft origin"))?;
    if is_localhost_name(host) {
        return Err(Error::MalformedEnvelope("invalid loft origin"));
    }
    let exact_loopback = is_numeric_loopback_host(host);
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && exact_loopback) {
        return Err(Error::MalformedEnvelope("invalid loft origin"));
    }
    if parsed.as_str().trim_end_matches('/') != origin {
        return Err(Error::MalformedEnvelope("noncanonical loft origin"));
    }
    Ok(())
}

fn payload(
    recipient: &[u8; 32],
    loft: &[u8; 32],
    loft_origin: &str,
    minute: u64,
    cursor: u64,
) -> Vec<u8> {
    let origin_len = u16::try_from(loft_origin.len()).expect("validated loft origin fits u16");
    let mut out = Vec::with_capacity(SIG_DOMAIN_FETCH.len() + 82 + loft_origin.len());
    out.extend_from_slice(SIG_DOMAIN_FETCH);
    out.extend_from_slice(recipient);
    out.extend_from_slice(loft);
    out.extend_from_slice(&origin_len.to_le_bytes());
    out.extend_from_slice(loft_origin.as_bytes());
    out.extend_from_slice(&minute.to_le_bytes());
    out.extend_from_slice(&cursor.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOFT: [u8; 32] = [9; 32];
    const OTHER_LOFT: [u8; 32] = [8; 32];
    const ORIGIN: &str = "https://loft.example";
    const OTHER_ORIGIN: &str = "https://other.example";
    const NOW: u64 = 29_768_428;

    #[test]
    fn a_valid_proof_authenticates_the_mailbox_owner() {
        let agent = Identity::from_seed([1; 32]);
        let auth = FetchAuth::new(&agent, &LOFT, ORIGIN, NOW, 0).unwrap();
        assert_eq!(
            auth.verify(&LOFT, ORIGIN, NOW).unwrap(),
            agent.verifying_key()
        );
    }

    #[test]
    fn nobody_can_drain_someone_elses_mailbox() {
        let agent = Identity::from_seed([1; 32]);
        let attacker = Identity::from_seed([2; 32]);

        // The attacker signs a proof naming the victim as recipient.
        let mut forged = FetchAuth::new(&attacker, &LOFT, ORIGIN, NOW, 0).unwrap();
        forged.recipient = agent.verifying_key().to_bytes();

        assert_eq!(forged.verify(&LOFT, ORIGIN, NOW), Err(Error::BadSignature));
    }

    #[test]
    fn a_proof_does_not_replay_to_another_loft() {
        let agent = Identity::from_seed([1; 32]);
        let auth = FetchAuth::new(&agent, &LOFT, ORIGIN, NOW, 0).unwrap();

        // Indistinguishable from any other rejection, by design.
        assert_eq!(
            auth.verify(&OTHER_LOFT, ORIGIN, NOW),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn a_proof_does_not_replay_to_another_origin_that_claims_the_same_key() {
        let agent = Identity::from_seed([1; 32]);
        let auth = FetchAuth::new(&agent, &LOFT, ORIGIN, NOW, 0).unwrap();

        assert_eq!(
            auth.verify(&LOFT, OTHER_ORIGIN, NOW),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn stale_and_future_proofs_are_refused() {
        let agent = Identity::from_seed([1; 32]);
        let auth = FetchAuth::new(&agent, &LOFT, ORIGIN, NOW, 0).unwrap();

        assert!(auth.verify(&LOFT, ORIGIN, NOW + CLOCK_SKEW_MINUTES).is_ok());
        assert!(auth.verify(&LOFT, ORIGIN, NOW - CLOCK_SKEW_MINUTES).is_ok());
        assert_eq!(
            auth.verify(&LOFT, ORIGIN, NOW + CLOCK_SKEW_MINUTES + 1),
            Err(Error::StaleTimestamp)
        );
        assert_eq!(
            auth.verify(&LOFT, ORIGIN, NOW - CLOCK_SKEW_MINUTES - 1),
            Err(Error::StaleTimestamp)
        );
    }

    #[test]
    fn the_cursor_is_covered_by_the_signature() {
        let agent = Identity::from_seed([1; 32]);
        let mut auth = FetchAuth::new(&agent, &LOFT, ORIGIN, NOW, 100).unwrap();
        auth.cursor = 0; // rewind, to re-read everything

        assert_eq!(auth.verify(&LOFT, ORIGIN, NOW), Err(Error::BadSignature));
    }

    #[test]
    fn debug_does_not_leak_the_fetch_credential() {
        let agent = Identity::from_seed([0xA7; 32]);
        let auth = FetchAuth::new(&agent, &LOFT, ORIGIN, NOW, 0xA7A7).unwrap();
        let recipient_canary = auth
            .recipient
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let signature_canary = auth
            .signature
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let debugged = format!("{auth:?}");

        assert_eq!(debugged, "FetchAuth(redacted)");
        assert!(!debugged.contains(ORIGIN));
        assert!(!debugged.contains(&recipient_canary[..8]));
        assert!(!debugged.contains(&signature_canary[..8]));
        assert!(!debugged.contains("42919"));
    }

    #[test]
    fn origins_are_bounded_canonical_and_transport_safe() {
        for invalid in [
            "",
            "http://loft.example",
            "http://localhost:7717",
            "https://localhost:7717",
            "https://localhost.:7717",
            "https://api.localhost:7717",
            "https://user@loft.example",
            "https://loft.example/path",
            "https://loft.example/",
            "HTTPS://loft.example",
        ] {
            assert!(validate_loft_origin(invalid).is_err(), "{invalid}");
        }
        assert!(validate_loft_origin("https://loft.example").is_ok());
        assert!(validate_loft_origin("http://127.0.0.1:7717").is_ok());
        assert!(validate_loft_origin("http://[::1]:7717").is_ok());
    }
}
