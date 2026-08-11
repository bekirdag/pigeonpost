//! Identity: one Ed25519 keypair per agent, plus the X25519 agreement key derived from it.
//!
//! One key, two uses. The X25519 key is the birational image of the Ed25519 key rather than a
//! second stored secret, so an agent has exactly one thing to back up and one thing to lose.
//! Cross-protocol confusion is prevented by domain separation in the HKDF `info` string
//! (`envelope.rs`), not by holding separate keys.

use curve25519_dalek::{montgomery::MontgomeryPoint, scalar::Scalar};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::ZeroizeOnDrop;

use crate::address::Address;
use crate::error::{Error, Result};

/// A 32-byte commitment to a successor public key: `SHA-256(successor_pubkey)`.
///
/// Published *before* it is needed. An attacker holding the operating key can only rotate to the
/// key this commits to, which they do not have — so a compromised key cannot steal the address.
/// See `docs/keys.md`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SuccessorCommitment(pub [u8; 32]);

impl SuccessorCommitment {
    pub fn for_key(pubkey: &VerifyingKey) -> Self {
        SuccessorCommitment(Sha256::digest(pubkey.as_bytes()).into())
    }

    /// Constant-time check that `candidate` is the committed successor.
    pub fn accepts(&self, candidate: &VerifyingKey) -> bool {
        use subtle::ConstantTimeEq;
        let computed = Sha256::digest(candidate.as_bytes());
        computed.ct_eq(&self.0).into()
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An agent's operating keypair.
///
/// The secret is zeroized on drop. It is never serialized: persistence is the client crate's
/// job, and it puts the secret in the OS keychain or a `0600` file (`sds.md` §5.2).
#[derive(ZeroizeOnDrop)]
pub struct Identity {
    #[zeroize(skip)]
    signing: SigningKey,
    seed: [u8; 32],
}

impl Identity {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// Rebuild an identity from its 32-byte seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Identity {
            signing: SigningKey::from_bytes(&seed),
            seed,
        }
    }

    /// The seed, for handing to a key store. Callers must zeroize their copy.
    pub fn to_seed(&self) -> [u8; 32] {
        self.seed
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn address(&self) -> Address {
        Address::from_pubkey(&self.verifying_key())
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing.sign(message)
    }

    /// The X25519 scalar corresponding to this Ed25519 key.
    pub(crate) fn agreement_scalar(&self) -> Scalar {
        // Ed25519 clamps SHA-512(seed)[..32] to produce its scalar; reproducing that here keeps
        // the scalar consistent with the Montgomery form of the public key.
        let hash = sha2::Sha512::digest(self.seed);
        let mut clamped = [0u8; 32];
        clamped.copy_from_slice(&hash[..32]);
        clamped[0] &= 248;
        clamped[31] &= 127;
        clamped[31] |= 64;
        Scalar::from_bytes_mod_order(clamped)
    }

    /// X25519 Diffie-Hellman against another agent's Ed25519 public key.
    pub(crate) fn agree(&self, peer: &VerifyingKey) -> Result<[u8; 32]> {
        let point = montgomery_of(peer)?;
        let shared = self.agreement_scalar() * point;
        // An all-zero output means a small-order peer point: reject rather than proceed.
        if shared == MontgomeryPoint([0u8; 32]) {
            return Err(Error::InvalidKey);
        }
        Ok(shared.to_bytes())
    }
}

impl core::fmt::Debug for Identity {
    /// Hand-written rather than derived: a derived `Debug` would print the seed, and secrets must
    /// never reach a log line (`sds.md` §9). Shows the public address only.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Identity({}, secret withheld)", self.address())
    }
}

/// Map an Ed25519 verifying key to its Montgomery (X25519) form.
pub(crate) fn montgomery_of(key: &VerifyingKey) -> Result<MontgomeryPoint> {
    Ok(key.to_montgomery())
}

/// This identity's X25519 public key, in Montgomery form.
///
/// Compliance keys (`docs/law.md` §3) are raw X25519 points rather than Ed25519 keys, so the
/// attribution path needs the Montgomery form directly instead of going through a `VerifyingKey`.
pub fn x25519_public(identity: &Identity) -> [u8; 32] {
    montgomery_of(&identity.verifying_key())
        .map(|point| point.to_bytes())
        .unwrap_or([0u8; 32])
}

/// Diffie-Hellman against a raw X25519 public key.
///
/// Unlike the identity-to-identity agreement path, the peer here is a Montgomery point that was
/// never an Ed25519 key —
/// a published compliance key. Small-order points are rejected rather than silently producing an
/// all-zero shared secret.
pub fn x25519_agree(identity: &Identity, peer: &[u8; 32]) -> Result<[u8; 32]> {
    let point = MontgomeryPoint(*peer);
    let shared = identity.agreement_scalar() * point;
    if shared == MontgomeryPoint([0u8; 32]) {
        return Err(Error::InvalidKey);
    }
    Ok(shared.to_bytes())
}

/// Parse an Ed25519 public key from its 32 raw bytes.
///
/// Invalid encodings and low-order keys are both rejected. A low-order key does not identify one
/// signing secret and can make non-strict Ed25519 verification accept attacker-chosen records.
pub fn verifying_key_from_bytes(bytes: &[u8; 32]) -> Result<VerifyingKey> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| Error::InvalidKey)?;
    if key.is_weak() {
        return Err(Error::InvalidKey);
    }
    Ok(key)
}

/// Strictly verify a detached signature.
///
/// Strict verification also rejects low-order public keys and signature `R` components when a
/// caller obtained a [`VerifyingKey`] without [`verifying_key_from_bytes`].
pub fn verify(pubkey: &VerifyingKey, message: &[u8], signature: &Signature) -> Result<()> {
    pubkey
        .verify_strict(message, signature)
        .map_err(|_| Error::BadSignature)
}

/// A freshly generated throwaway keypair, used once to wrap a single message so that the
/// wrapper carries no link to the sender (`envelope.rs`).
pub(crate) fn ephemeral() -> Identity {
    Identity::generate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffie_hellman_agrees_in_both_directions() {
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);

        let ab = alice.agree(&bob.verifying_key()).unwrap();
        let ba = bob.agree(&alice.verifying_key()).unwrap();

        assert_eq!(ab, ba, "ECDH must be symmetric or nothing decrypts");
        assert_ne!(ab, [0u8; 32]);
    }

    #[test]
    fn different_pairs_produce_different_secrets() {
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);
        let eve = Identity::from_seed([3; 32]);

        assert_ne!(
            alice.agree(&bob.verifying_key()).unwrap(),
            alice.agree(&eve.verifying_key()).unwrap()
        );
    }

    #[test]
    fn seed_round_trips_to_the_same_identity() {
        let original = Identity::generate();
        let seed = original.to_seed();
        let restored = Identity::from_seed(seed);
        assert_eq!(original.verifying_key(), restored.verifying_key());
        assert_eq!(original.address(), restored.address());
    }

    #[test]
    fn signatures_verify_and_reject_tampering() {
        let id = Identity::from_seed([4; 32]);
        let sig = id.sign(b"a message");
        assert!(verify(&id.verifying_key(), b"a message", &sig).is_ok());
        assert_eq!(
            verify(&id.verifying_key(), b"a different message", &sig),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn weak_public_keys_are_rejected_before_verification() {
        let mut weak = [0u8; 32];
        weak[0] = 1;
        let parsed = VerifyingKey::from_bytes(&weak).expect("dalek accepts the encoded point");
        assert!(parsed.is_weak());
        assert_eq!(verifying_key_from_bytes(&weak), Err(Error::InvalidKey));

        let signature = Signature::from_bytes(&[0u8; 64]);
        assert_eq!(
            verify(&parsed, b"any record", &signature),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn successor_commitment_accepts_only_the_committed_key() {
        let successor = Identity::from_seed([5; 32]);
        let impostor = Identity::from_seed([6; 32]);
        let commitment = SuccessorCommitment::for_key(&successor.verifying_key());

        assert!(commitment.accepts(&successor.verifying_key()));
        assert!(
            !commitment.accepts(&impostor.verifying_key()),
            "this is the property that stops a compromised key stealing an address"
        );
    }
}
