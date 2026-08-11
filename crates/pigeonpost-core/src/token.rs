//! Capability tokens — the open-inbox answer from `docs/spam.md`.
//!
//! Publish `/k/…#t=readme` instead of a bare address. The recipient mints tokens locally and
//! registers their *hashes* with its lofts, so a loft can gate open-inbox mail without learning
//! anything about senders. Harvested token? Revoke it and edit one line of the README.
//!
//! Presentation is **loft-bound**: a sender presents
//! `H(token ‖ loft_pubkey ‖ canonical_loft_origin)`, not the token. Binding both coordinates keeps
//! a hostile endpoint from claiming an honest loft's public key and relaying the credential there.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const TOKEN_DOMAIN: &[u8] = b"pigeonpost/token/v1";
const BOUND_DOMAIN: &[u8] = b"pigeonpost/token-bound/v2";

/// A capability token. Published with an address; held by senders.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 32]);

impl Token {
    /// Derive the token for `label` from the recipient's token secret.
    ///
    /// Deterministic, so a recipient never has to store its tokens — only its secret and the
    /// list of labels it has not revoked.
    pub fn mint(secret: &[u8; 32], label: &str) -> Self {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
        mac.update(TOKEN_DOMAIN);
        mac.update(label.as_bytes());
        Token(mac.finalize().into_bytes().into())
    }

    /// The short form published alongside an address: `#t=<hex>`.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let s = core::str::from_utf8(chunk).ok()?;
            bytes[i] = u8::from_str_radix(s, 16).ok()?;
        }
        Some(Token(bytes))
    }

    /// What a sender presents to a specific loft.
    pub fn presentation(
        &self,
        loft_pubkey: &[u8; 32],
        loft_origin: &str,
    ) -> crate::Result<Presentation> {
        crate::fetch_auth::validate_loft_origin(loft_origin)?;
        let origin_len = u16::try_from(loft_origin.len()).expect("validated loft origin fits u16");
        let mut hasher = Sha256::new();
        hasher.update(BOUND_DOMAIN);
        hasher.update(self.0);
        hasher.update(loft_pubkey);
        hasher.update(origin_len.to_le_bytes());
        hasher.update(loft_origin.as_bytes());
        Ok(Presentation(hasher.finalize().into()))
    }
}

impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // A token is a bearer credential; keep it out of logs.
        f.write_str("Token(redacted)")
    }
}

/// The loft-bound value a sender sends and a loft stores. Reveals neither the token nor the
/// sender.
#[derive(Clone, PartialEq, Eq)]
pub struct Presentation([u8; 32]);

impl core::fmt::Debug for Presentation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // A presentation is a replayable, loft-bound bearer credential.
        f.write_str("Presentation(redacted)")
    }
}

impl Presentation {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Rebuild from stored bytes, for comparing against a loft's registered set.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Presentation(bytes)
    }

    /// Constant-time comparison against a registered value.
    pub fn matches(&self, registered: &Presentation) -> bool {
        self.0.ct_eq(&registered.0).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7; 32];
    const LOFT_A: [u8; 32] = [1; 32];
    const LOFT_B: [u8; 32] = [2; 32];
    const ORIGIN_A: &str = "https://a.example";
    const ORIGIN_B: &str = "https://b.example";

    #[test]
    fn minting_is_deterministic() {
        assert_eq!(
            Token::mint(&SECRET, "readme"),
            Token::mint(&SECRET, "readme")
        );
    }

    #[test]
    fn different_labels_give_different_tokens() {
        assert_ne!(Token::mint(&SECRET, "readme"), Token::mint(&SECRET, "docs"));
    }

    #[test]
    fn different_secrets_give_different_tokens() {
        assert_ne!(
            Token::mint(&SECRET, "readme"),
            Token::mint(&[8; 32], "readme")
        );
    }

    #[test]
    fn presentations_are_loft_bound() {
        let token = Token::mint(&SECRET, "readme");
        let at_a = token.presentation(&LOFT_A, ORIGIN_A).unwrap();
        let at_b = token.presentation(&LOFT_B, ORIGIN_B).unwrap();

        assert!(
            !at_a.matches(&at_b),
            "a presentation must not replay to another loft"
        );
        assert!(at_a.matches(&token.presentation(&LOFT_A, ORIGIN_A).unwrap()));
    }

    #[test]
    fn a_claimed_honest_key_does_not_make_a_presentation_replayable_across_origins() {
        let token = Token::mint(&SECRET, "readme");
        let at_a = token.presentation(&LOFT_A, ORIGIN_A).unwrap();
        let at_b_with_same_key = token.presentation(&LOFT_A, ORIGIN_B).unwrap();

        assert!(!at_a.matches(&at_b_with_same_key));
    }

    #[test]
    fn hex_round_trips() {
        let token = Token::mint(&SECRET, "conf-talk");
        let hex = token.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(Token::from_hex(&hex), Some(token));
    }

    #[test]
    fn malformed_hex_is_refused() {
        assert_eq!(Token::from_hex("abc"), None);
        assert_eq!(Token::from_hex(&"z".repeat(64)), None);
    }

    #[test]
    fn debug_does_not_leak_the_token() {
        let token = Token::mint(&SECRET, "readme");
        let debugged = format!("{token:?}");
        assert!(!debugged.contains(&token.to_hex()[..8]));
    }

    #[test]
    fn debug_does_not_leak_a_bound_presentation() {
        let presentation = Token::mint(&SECRET, "debug-canary")
            .presentation(&LOFT_A, ORIGIN_A)
            .unwrap();
        let canary = presentation
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        assert_eq!(format!("{presentation:?}"), "Presentation(redacted)");
        assert!(!format!("{presentation:?}").contains(&canary[..8]));
    }

    #[test]
    fn revocation_is_just_forgetting_a_label() {
        // The loft holds registered presentations; dropping one revokes that surface only.
        let readme = Token::mint(&SECRET, "readme")
            .presentation(&LOFT_A, ORIGIN_A)
            .unwrap();
        let docs = Token::mint(&SECRET, "docs")
            .presentation(&LOFT_A, ORIGIN_A)
            .unwrap();
        let registered = [docs.clone()];

        assert!(!registered.iter().any(|r| readme.matches(r)));
        assert!(registered.iter().any(|r| docs.matches(r)));
    }
}
