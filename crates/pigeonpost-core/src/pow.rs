//! Hashcash stamps (NIP-13's design, our own bytes).
//!
//! Difficulty is leading zero bits of `SHA-256(domain ‖ wrap_id ‖ nonce)`. Seconds of CPU for one
//! genuine message; ruinous for a million. PoW alone is not a spam solution — it is the rate
//! limiter that makes the other four layers in `spam.md` affordable.
//!
//! A loft enforces a **flat per-recipient floor**, never a per-sender one: the wrap hides sender
//! identity from the loft, so the tier gradient in `spam.md` is applied client-side after unwrap.

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

const POW_DOMAIN: &[u8] = b"pigeonpost/pow/v1";

/// Count of leading zero bits in `SHA-256(domain ‖ id ‖ nonce)`.
pub fn work(id: &[u8; 32], nonce: u64) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(POW_DOMAIN);
    hasher.update(id);
    hasher.update(nonce.to_le_bytes());
    leading_zero_bits(&hasher.finalize())
}

/// Search for a nonce meeting `difficulty`, giving up after `max_attempts`.
///
/// Bounded rather than looping forever: a caller handed an absurd difficulty by a hostile
/// recipient policy should fail, not hang.
pub fn mine(id: &[u8; 32], difficulty: u32, max_attempts: u64) -> Option<u64> {
    (0..max_attempts).find(|&nonce| work(id, nonce) >= difficulty)
}

/// Verify a stamp meets the required difficulty. Cheap: one hash.
pub fn verify(id: &[u8; 32], nonce: u64, difficulty: u32) -> Result<()> {
    if difficulty == 0 {
        return Ok(());
    }
    if work(id, nonce) >= difficulty {
        Ok(())
    } else {
        Err(Error::InsufficientWork)
    }
}

fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut total = 0;
    for &byte in digest {
        if byte == 0 {
            total += 8;
        } else {
            total += byte.leading_zeros();
            break;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [u8; 32] = [42; 32];

    #[test]
    fn mined_nonces_verify() {
        for difficulty in [1u32, 4, 8, 12] {
            let nonce = mine(&ID, difficulty, 5_000_000).expect("findable at this difficulty");
            assert!(verify(&ID, nonce, difficulty).is_ok(), "d={difficulty}");
        }
    }

    #[test]
    fn zero_difficulty_accepts_anything() {
        assert!(verify(&ID, 0, 0).is_ok());
        assert!(verify(&ID, 12345, 0).is_ok());
    }

    #[test]
    fn insufficient_work_is_rejected() {
        let nonce = mine(&ID, 4, 1_000_000).unwrap();
        // A nonce good for 4 bits is almost never good for 32.
        assert_eq!(verify(&ID, nonce, 32), Err(Error::InsufficientWork));
    }

    #[test]
    fn a_stamp_does_not_transfer_to_another_message() {
        let nonce = mine(&ID, 10, 5_000_000).unwrap();
        let other_id = [43u8; 32];
        assert!(
            verify(&other_id, nonce, 10).is_err(),
            "stamps must be bound to the message for which they were mined"
        );
    }

    #[test]
    fn mining_gives_up_rather_than_hanging() {
        assert_eq!(mine(&ID, 250, 5_000), None);
    }

    #[test]
    fn leading_zero_bits_counts_correctly() {
        assert_eq!(leading_zero_bits(&[0xff, 0x00]), 0);
        assert_eq!(leading_zero_bits(&[0x7f]), 1);
        assert_eq!(leading_zero_bits(&[0x0f]), 4);
        assert_eq!(leading_zero_bits(&[0x00, 0xff]), 8);
        assert_eq!(leading_zero_bits(&[0x00, 0x0f]), 12);
        assert_eq!(leading_zero_bits(&[0x00, 0x00]), 16);
    }
}
