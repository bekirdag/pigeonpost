//! Property tests: hostile bytes into every parser and into message processing must never
//! panic, and a mutated message must never decrypt to anything but the original.

use pigeonpost_groupcrypto::*;
use proptest::prelude::*;

const G: &str = "grp_fuzz";

struct Fixture {
    bob_state: Vec<u8>,
    key: Vec<u8>,
    message: Vec<u8>,
    commit: Vec<u8>,
    welcome: Vec<u8>,
    key_package: Vec<u8>,
    carol_state: Vec<u8>,
}

fn fixture() -> &'static Fixture {
    static F: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let mut alice = Device::new_with_identity_seed(&[1; 32], "a").unwrap();
        let mut bob = Device::new_with_identity_seed(&[2; 32], "b").unwrap();
        let mut carol = Device::new_with_identity_seed(&[3; 32], "c").unwrap();
        let kp = bob.generate_key_packages(1).unwrap();
        alice.create_group(G).unwrap();
        let c = alice.add_members(G, &kp).unwrap();
        alice.confirm_commit(G).unwrap();
        bob.join_from_welcome(c.welcome.as_ref().unwrap()).unwrap();
        let key = generate_state_key().unwrap();
        let bob_state = bob.export_state(&key).unwrap();
        let message = alice
            .encrypt_message(G, "m-1", b"hello fuzz", &[])
            .unwrap()
            .ciphertext;
        let carol_kp = carol.generate_key_packages(1).unwrap().remove(0);
        let commit = alice
            .add_members(G, std::slice::from_ref(&carol_kp))
            .unwrap();
        let carol_state = carol.export_state(&key).unwrap();
        Fixture {
            bob_state,
            key,
            message,
            commit: commit.commit,
            welcome: commit.welcome.unwrap(),
            key_package: carol_kp,
            carol_state,
        }
    })
}

fn mutate(base: &[u8], edits: &[(usize, u8)], cut: Option<usize>) -> Vec<u8> {
    let mut out = base.to_vec();
    for (i, x) in edits {
        let i = i % out.len();
        out[i] ^= x | 1;
    }
    if let Some(c) = cut {
        out.truncate(c % (out.len() + 1));
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let f = fixture();
        let mut bob = Device::import_state(&f.bob_state, &f.key).unwrap();
        let _ = bob.process_incoming(G, &bytes);
        let _ = bob.join_from_welcome(&bytes);
        let _ = inspect_envelope(&bytes);
        let _ = inspect_key_package(&bytes);
        let _ = welcome_recipients(&bytes);
        let _ = MessageAad::decode(&bytes);
        let _ = Device::import_state(&bytes, &f.key);
        let _ = bob.add_members(G, std::slice::from_ref(&bytes));
    }

    #[test]
    fn mutated_application_message(edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..4), cut in proptest::option::of(any::<usize>())) {
        let f = fixture();
        let mut bob = Device::import_state(&f.bob_state, &f.key).unwrap();
        let bad = mutate(&f.message, &edits, cut);
        if let Ok(Incoming::Message(m)) = bob.process_incoming(G, &bad) {
            prop_assert_eq!(m.body, b"hello fuzz".to_vec());
        }
        // The original still decrypts on a fresh copy (or after a rejected forgery).
        let mut fresh = Device::import_state(&f.bob_state, &f.key).unwrap();
        prop_assert!(matches!(fresh.process_incoming(G, &f.message), Ok(Incoming::Message(_))));
    }

    #[test]
    fn mutated_handshake_objects(edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..4), cut in proptest::option::of(any::<usize>())) {
        let f = fixture();
        let mut bob = Device::import_state(&f.bob_state, &f.key).unwrap();
        let _ = bob.process_incoming(G, &mutate(&f.commit, &edits, cut));
        let mut carol = Device::import_state(&f.carol_state, &f.key).unwrap();
        if let Ok(joined) = carol.join_from_welcome(&mutate(&f.welcome, &edits, cut)) {
            prop_assert_eq!(joined.group_id, G);
            prop_assert_eq!(joined.members.len(), 3);
        }
        let mut carol = Device::import_state(&f.carol_state, &f.key).unwrap();
        prop_assert!(carol.join_from_welcome(&f.welcome).is_ok());
        if let Ok(info) = inspect_key_package(&mutate(&f.key_package, &edits, cut)) {
            // Anything that still verifies must still be carol's attested device.
            prop_assert_eq!(info.device_id, "c");
        }
    }
}
