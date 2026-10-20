//! The local `/k/` derivation must match pigeonpost-core's exactly.
use pigeonpost_core::address::Address;
use pigeonpost_groupcrypto::credential::{address_for_key, canonical_mailbox};
use proptest::prelude::*;

proptest! {
    #[test]
    fn matches_core(seed in any::<[u8; 32]>()) {
        let key = ed25519_dalek::SigningKey::from_bytes(&seed).verifying_key();
        let ours = address_for_key(&key.to_bytes());
        let core = Address::from_pubkey(&key);
        prop_assert_eq!(ours.as_str(), core.as_str());
        prop_assert_eq!(canonical_mailbox(&ours).unwrap(), ours.clone());
        // Every non-canonical spelling core accepts is refused here.
        let upper = ours.to_uppercase().replacen("/K/", "/k/", 1);
        prop_assert!(canonical_mailbox(&upper).is_err() || upper == ours);
    }

    #[test]
    fn canonical_agrees_with_core(body in "[0-9a-z]{26}") {
        let candidate = format!("/k/{body}");
        let core_canonical = Address::parse(&candidate).map(|a| a.as_str() == candidate).unwrap_or(false);
        prop_assert_eq!(canonical_mailbox(&candidate).is_ok(), core_canonical);
    }
}
