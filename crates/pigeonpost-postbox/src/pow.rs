//! Proof-of-work anti-abuse for anonymous identity creation (plan §14.1).
//!
//! A botnet must not be able to mint millions of `/k/` inboxes for free. Before an anonymous caller
//! may create an identity, it fetches a **challenge** and returns a **solution** (a nonce) whose
//! `SHA-256(challenge : solution)` has at least `bits` leading zero bits — hashcash. Honest single
//! use is ~instant; mass creation gets exponentially expensive as difficulty rises with load.
//!
//! Challenges are **HMAC-signed and self-describing**, so the server keeps no per-challenge state to
//! issue them. Single-use is enforced by a small in-memory set of spent challenges, pruned by expiry
//! (a short TTL bounds its size). Authenticated callers skip PoW entirely.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Why a submitted proof-of-work was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PowError {
    #[error("malformed challenge")]
    Malformed,
    #[error("challenge signature is invalid")]
    BadSignature,
    #[error("challenge has expired")]
    Expired,
    #[error("solution does not meet the required difficulty")]
    Insufficient,
    #[error("challenge has already been spent")]
    Replay,
}

/// A verified proof: the difficulty it satisfied and when the challenge expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    pub bits: u32,
    pub exp: u64,
}

/// Issues and verifies proof-of-work challenges. Cheap to clone-by-reference behind an `Arc`.
pub struct Pow {
    secret: Vec<u8>,
    min_bits: u32,
    max_bits: u32,
    ttl_secs: u64,
    spent: Mutex<HashMap<String, u64>>, // challenge -> exp; pruned on access
}

impl Pow {
    pub fn new(secret: Vec<u8>, min_bits: u32, max_bits: u32, ttl_secs: u64) -> Self {
        let (min_bits, max_bits) = if min_bits <= max_bits {
            (min_bits, max_bits)
        } else {
            (max_bits, min_bits)
        };
        Pow {
            secret,
            min_bits,
            max_bits,
            ttl_secs: ttl_secs.max(1),
            spent: Mutex::new(HashMap::new()),
        }
    }

    /// Public accessor kept for tests and the future adaptive path; the server issues via
    /// [`Self::difficulty_for_rate`].
    #[allow(dead_code)]
    pub fn min_bits(&self) -> u32 {
        self.min_bits
    }

    /// Difficulty for the current creation rate: `min_bits` when idle, climbing one bit per
    /// `threshold` recent creations, clamped to `max_bits`. One extra bit doubles the work.
    pub fn difficulty_for_rate(&self, recent: u64, threshold: u64) -> u32 {
        if threshold == 0 {
            return self.max_bits;
        }
        let over = (recent / threshold) as u32;
        self.min_bits.saturating_add(over).min(self.max_bits)
    }

    /// Mint a challenge string `nonce.exp.bits.sig`, valid for `ttl_secs`.
    pub fn issue(&self, bits: u32, now: u64) -> String {
        let bits = bits.clamp(self.min_bits, self.max_bits);
        let mut nonce = [0u8; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
        let payload = format!("{}.{}.{}", hex(&nonce), now + self.ttl_secs, bits);
        let sig = self.sign(payload.as_bytes());
        format!("{payload}.{sig}")
    }

    /// The difficulty embedded in a challenge, ignoring signature/expiry. A client/test helper so a
    /// solver knows the target; not called server-side (the server trusts its own signed challenge).
    #[allow(dead_code)]
    pub fn bits_of(challenge: &str) -> Option<u32> {
        challenge.split('.').nth(2)?.parse().ok()
    }

    /// The expiry embedded in a challenge — for reporting only.
    pub fn exp_of(challenge: &str) -> Option<u64> {
        challenge.split('.').nth(1)?.parse().ok()
    }

    /// Stateless check: signature valid, not expired, and the solution meets difficulty.
    pub fn verify(&self, challenge: &str, solution: &str, now: u64) -> Result<Verified, PowError> {
        let (payload, sig) = challenge.rsplit_once('.').ok_or(PowError::Malformed)?;
        let expected = self.sign(payload.as_bytes());
        if !ct_eq(sig.as_bytes(), expected.as_bytes()) {
            return Err(PowError::BadSignature);
        }
        let mut parts = payload.split('.');
        let _nonce = parts.next().ok_or(PowError::Malformed)?;
        let exp: u64 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(PowError::Malformed)?;
        let bits: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(PowError::Malformed)?;
        if parts.next().is_some() {
            return Err(PowError::Malformed);
        }
        if now > exp {
            return Err(PowError::Expired);
        }
        let mut h = Sha256::new();
        h.update(challenge.as_bytes());
        h.update(b":");
        h.update(solution.as_bytes());
        if leading_zero_bits(&h.finalize()) < bits {
            return Err(PowError::Insufficient);
        }
        Ok(Verified { bits, exp })
    }

    /// `verify`, then burn the challenge so a valid (challenge, solution) can't be replayed within
    /// its TTL. Expired entries are pruned on each call, bounding the set to one TTL window.
    pub fn consume(&self, challenge: &str, solution: &str, now: u64) -> Result<Verified, PowError> {
        let verified = self.verify(challenge, solution, now)?;
        let mut spent = self.spent.lock().expect("pow spent lock");
        spent.retain(|_, exp| *exp > now);
        if spent.insert(challenge.to_string(), verified.exp).is_some() {
            return Err(PowError::Replay);
        }
        Ok(verified)
    }

    fn sign(&self, payload: &[u8]) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(payload);
        hex(&mac.finalize().into_bytes())
    }
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0;
    for &b in bytes {
        if b == 0 {
            count += 8;
        } else {
            count += b.leading_zeros();
            break;
        }
    }
    count
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.ct_eq(b).into()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pow() -> Pow {
        Pow::new(b"test-secret".to_vec(), 8, 24, 120)
    }

    /// Brute-force a solution for a low difficulty (fast for tests).
    fn solve(challenge: &str) -> String {
        for n in 0u64.. {
            let sol = n.to_string();
            let mut h = Sha256::new();
            h.update(challenge.as_bytes());
            h.update(b":");
            h.update(sol.as_bytes());
            if leading_zero_bits(&h.finalize()) >= Pow::bits_of(challenge).unwrap() {
                return sol;
            }
        }
        unreachable!()
    }

    #[test]
    fn round_trip_verifies_and_consumes() {
        let pow = test_pow();
        let ch = pow.issue(8, 1_000);
        let sol = solve(&ch);
        assert!(pow.verify(&ch, &sol, 1_000).is_ok());
        assert!(pow.consume(&ch, &sol, 1_000).is_ok());
        // second consume of the same challenge is a replay
        assert_eq!(pow.consume(&ch, &sol, 1_000), Err(PowError::Replay));
    }

    #[test]
    fn wrong_solution_is_insufficient() {
        let pow = test_pow();
        let ch = pow.issue(16, 1_000);
        assert_eq!(
            pow.verify(&ch, "not-a-solution", 1_000),
            Err(PowError::Insufficient)
        );
    }

    #[test]
    fn expired_challenge_rejected() {
        let pow = test_pow();
        let ch = pow.issue(8, 1_000); // exp = 1_120
        let sol = solve(&ch);
        assert_eq!(pow.verify(&ch, &sol, 2_000), Err(PowError::Expired));
    }

    #[test]
    fn tampered_signature_rejected() {
        let pow = test_pow();
        let ch = pow.issue(8, 1_000);
        let sol = solve(&ch);
        // flip the difficulty in the payload; signature no longer matches
        let mut parts: Vec<&str> = ch.split('.').collect();
        parts[2] = "4";
        let forged = parts.join(".");
        assert_eq!(
            pow.verify(&forged, &sol, 1_000),
            Err(PowError::BadSignature)
        );
    }

    #[test]
    fn difficulty_climbs_with_rate() {
        let pow = test_pow(); // min 8, max 24
        assert_eq!(pow.difficulty_for_rate(0, 100), 8);
        assert_eq!(pow.difficulty_for_rate(500, 100), 13);
        assert_eq!(pow.difficulty_for_rate(10_000, 100), 24); // clamped to max
    }
}
