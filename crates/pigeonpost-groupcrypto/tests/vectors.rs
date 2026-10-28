//! Deterministic known-answer vectors for the Pigeonpost-specific glue in this crate (everything
//! OpenMLS does not define): mailbox addresses, device attestations, credential identity bytes,
//! device fingerprints and their digits, group safety numbers, the authenticated data (AAD),
//! the detached envelope signature and the sealed attachment format.
//!
//! The expected values live in `tests/vectors/groupcrypto-v1.txt` (one `name = value` per line,
//! hex unless noted) so that an independent implementation or reviewer can check them without
//! running Rust. This test recomputes every value from fixed inputs and compares.
//!
//! Where a value is built here from the documented layout (rather than by a crate function), the
//! test also feeds it back through the crate's public parser/verifier, so the vector pins the
//! crate's behaviour and the specification in `docs/group-e2ee.md` at the same time.
//!
//! Regenerate (only after a deliberate, versioned format change):
//! `PPGC_WRITE_VECTORS=1 cargo test --release --locked --test vectors`

use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signer as _, SigningKey};
use pigeonpost_groupcrypto::{
    attestation_payload, device_fingerprint, fingerprint_digits, mailbox_for_seed, open_attachment,
    safety_number, sign_attestation_with_seed, verify_envelope_signature, AttachmentRef,
    MessageAad, MessageKind,
};
use sha2::{Digest, Sha256};

const FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/vectors/groupcrypto-v1.txt"
);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn seed(fill: impl Fn(u8) -> u8) -> [u8; 32] {
    let mut s = [0u8; 32];
    for (i, b) in s.iter_mut().enumerate() {
        *b = fill(i as u8);
    }
    s
}

fn u16be(n: usize) -> [u8; 2] {
    (n as u16).to_be_bytes()
}

fn compute() -> BTreeMap<String, String> {
    let mut v = BTreeMap::new();
    let mut put = |k: &str, val: String| {
        v.insert(k.to_owned(), val);
    };

    // --- Inputs ------------------------------------------------------------------------------
    // Mailbox identity seeds (ed25519 seeds as pigeonpost-core stores them).
    let alice_seed = seed(|i| i + 1); // 01 02 .. 20
    let bob_seed = seed(|_| 0x42);
    let carol_seed = seed(|i| 0xff - i);
    // Device MLS signature keys (ed25519) for one device per mailbox.
    let alice_dev_seed = seed(|_| 0x07);
    let bob_dev_seed = seed(|i| i.wrapping_mul(3));
    let carol_dev_seed = seed(|i| 0x80 ^ i);
    put("input.alice_identity_seed", hex(&alice_seed));
    put("input.bob_identity_seed", hex(&bob_seed));
    put("input.carol_identity_seed", hex(&carol_seed));
    put("input.alice_device_seed", hex(&alice_dev_seed));
    put("input.bob_device_seed", hex(&bob_dev_seed));
    put("input.carol_device_seed", hex(&carol_dev_seed));
    put("input.alice_device_id", "phone-1".into());
    put("input.bob_device_id", "laptop".into());
    put("input.carol_device_id", "agentd.main".into());

    let pk = |s: &[u8; 32]| SigningKey::from_bytes(s).verifying_key().to_bytes();

    // --- 1. Mailbox addresses (/k/ + Crockford base32 of SHA-256(pk)[..16]) -------------------
    let alice = mailbox_for_seed(&alice_seed);
    let bob = mailbox_for_seed(&bob_seed);
    let carol = mailbox_for_seed(&carol_seed);
    put("address.alice.public_key", hex(&pk(&alice_seed)));
    put("address.alice", alice.clone());
    put("address.bob.public_key", hex(&pk(&bob_seed)));
    put("address.bob", bob.clone());
    put("address.carol.public_key", hex(&pk(&carol_seed)));
    put("address.carol", carol.clone());

    // --- 2. Device attestation payload and signature -----------------------------------------
    let alice_dev_pk = pk(&alice_dev_seed);
    let payload = attestation_payload(&alice, "phone-1", &alice_dev_pk).unwrap();
    // Layout: domain || u16 len || mailbox || u16 len || device id || u16 suite || u16 len || key
    let mut expected = b"pigeonpost/groupcrypto/device-attestation/v1\0".to_vec();
    expected.extend(u16be(alice.len()));
    expected.extend(alice.as_bytes());
    expected.extend(u16be(7));
    expected.extend(b"phone-1");
    expected.extend(0x0001u16.to_be_bytes());
    expected.extend(u16be(32));
    expected.extend(alice_dev_pk);
    assert_eq!(payload, expected, "attestation payload layout");
    let attestation = sign_attestation_with_seed(&alice_seed, &payload).unwrap();
    put("attestation.alice.device_public_key", hex(&alice_dev_pk));
    put("attestation.alice.payload", hex(&payload));
    put("attestation.alice.signature", hex(&attestation));

    // --- 3. Credential identity bytes (BasicCredential identity of every leaf) ---------------
    // u8 version=1 || u16 len || mailbox || u16 len || device id || pk[32] || sig[64]
    let mut credential = vec![1u8];
    credential.extend(u16be(alice.len()));
    credential.extend(alice.as_bytes());
    credential.extend(u16be(7));
    credential.extend(b"phone-1");
    credential.extend(pk(&alice_seed));
    credential.extend(attestation);
    put("credential.alice.identity", hex(&credential));

    // --- 4. Device fingerprints and digits ---------------------------------------------------
    let devices = [
        (&alice, "phone-1", pk(&alice_dev_seed), "alice"),
        (&bob, "laptop", pk(&bob_dev_seed), "bob"),
        (&carol, "agentd.main", pk(&carol_dev_seed), "carol"),
    ];
    let mut fps = Vec::new();
    for (mailbox, device, key, name) in &devices {
        let fp = device_fingerprint(mailbox, device, key);
        put(&format!("fingerprint.{name}"), hex(&fp));
        put(
            &format!("fingerprint.{name}.digits"),
            fingerprint_digits(&fp),
        );
        fps.push(fp);
    }

    // --- 5. Safety numbers (order-independent, duplicates ignored) ---------------------------
    let two = safety_number(&fps[..2]);
    let three = safety_number(&fps);
    let shuffled = [fps[2], fps[0], fps[1], fps[0]];
    assert_eq!(
        safety_number(&shuffled),
        three,
        "order/duplicate independent"
    );
    put("safety_number.alice_bob", two);
    put("safety_number.alice_bob_carol", three);
    put("safety_number.empty", safety_number(&[]));

    // --- 6. Authenticated data (AAD) of a protected message ---------------------------------
    // "PPGC" || u8 1 || u8 kind || u16 len || group id || u16 len || sender mailbox
    //        || u16 len || sender device || u64 epoch || u16 len || client message id
    let group = "grp_0123456789abcdefghjkmnpqrs";
    let mut aad = b"PPGC".to_vec();
    aad.push(1);
    aad.push(MessageKind::Application as u8);
    aad.extend(u16be(group.len()));
    aad.extend(group.as_bytes());
    aad.extend(u16be(alice.len()));
    aad.extend(alice.as_bytes());
    aad.extend(u16be(7));
    aad.extend(b"phone-1");
    aad.extend(7u64.to_be_bytes());
    aad.extend(u16be(10));
    aad.extend(b"cm_0000001");
    let parsed = MessageAad::decode(&aad).unwrap();
    assert_eq!(parsed.kind, MessageKind::Application);
    assert_eq!(parsed.group_id, group.as_bytes());
    assert_eq!(parsed.sender_mailbox, alice);
    assert_eq!(parsed.sender_device, "phone-1");
    assert_eq!(parsed.epoch, 7);
    assert_eq!(parsed.client_message_id, "cm_0000001");
    let mut trailing = aad.clone();
    trailing.push(0);
    assert!(
        MessageAad::decode(&trailing).is_err(),
        "trailing byte refused"
    );
    let mut commit = aad.clone();
    commit[5] = MessageKind::Commit as u8;
    assert_eq!(
        MessageAad::decode(&commit).unwrap().kind,
        MessageKind::Commit
    );
    commit[5] = 3;
    assert!(MessageAad::decode(&commit).is_err(), "unknown kind refused");
    put("aad.application.group_id", group.into());
    put("aad.application.epoch", "7".into());
    put("aad.application.client_message_id", "cm_0000001".into());
    put("aad.application", hex(&aad));

    // --- 7. Detached envelope signature ------------------------------------------------------
    // Signed input: "pigeonpost/groupcrypto/envelope/v1\0" || SHA-256(serialized MLS message).
    // The vector signs a fixed stand-in for the MLS bytes; the server-side verifier is generic.
    let message = b"stand-in for a serialized MLS PrivateMessage";
    let mut input = b"pigeonpost/groupcrypto/envelope/v1\0".to_vec();
    input.extend(Sha256::digest(message));
    let sig = SigningKey::from_bytes(&alice_dev_seed)
        .sign(&input)
        .to_bytes();
    verify_envelope_signature(&alice_dev_pk, message, &sig).expect("envelope verifies");
    let mut other = message.to_vec();
    other[0] ^= 1;
    assert!(verify_envelope_signature(&alice_dev_pk, &other, &sig).is_err());
    put(
        "envelope.message_ascii",
        String::from_utf8(message.to_vec()).unwrap(),
    );
    put("envelope.signing_input", hex(&input));
    put("envelope.signature", hex(&sig));

    // --- 8. Sealed attachment ---------------------------------------------------------------
    // "PPGA" || u8 1 || nonce[24] || XChaCha20-Poly1305(key, nonce, plaintext, aad = 5-byte header)
    let key = seed(|i| 0xa0 + i);
    let nonce: [u8; 24] = core::array::from_fn(|i| 0x10 + i as u8);
    let plaintext = b"hello, group";
    let header = *b"PPGA\x01";
    let ct = XChaCha20Poly1305::new((&key).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .unwrap();
    let mut sealed = header.to_vec();
    sealed.extend(nonce);
    sealed.extend(ct);
    let reference = AttachmentRef {
        key,
        sealed_sha256: Sha256::digest(&sealed).into(),
        plaintext_len: plaintext.len() as u64,
    };
    assert_eq!(open_attachment(&sealed, &reference).unwrap(), plaintext);
    let mut tampered = sealed.clone();
    tampered[4] = 2; // version byte is authenticated
    assert!(open_attachment(&tampered, &reference).is_err());
    put("attachment.key", hex(&key));
    put("attachment.nonce", hex(&nonce));
    put("attachment.plaintext_ascii", "hello, group".into());
    put("attachment.sealed", hex(&sealed));
    put("attachment.sealed_sha256", hex(&reference.sealed_sha256));

    v
}

fn render(v: &BTreeMap<String, String>) -> String {
    let mut out = String::from(
        "# Pigeonpost groupcrypto known-answer vectors, format version 1 (ENC_SCHEME ...+ppgc1).\n\
         # Generated and checked by crates/pigeonpost-groupcrypto/tests/vectors.rs; see\n\
         # docs/security/groupcrypto-review-packet.md. Values are lowercase hex unless the name\n\
         # ends in _ascii, is an address, a device id, digits or a safety number.\n",
    );
    for (k, val) in v {
        out.push_str(&format!("{k} = {val}\n"));
    }
    out
}

#[test]
fn known_answer_vectors() {
    let computed = compute();
    let rendered = render(&computed);
    if std::env::var_os("PPGC_WRITE_VECTORS").is_some() {
        std::fs::write(FILE, &rendered).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(FILE).expect("committed vector file");
    let committed = committed.replace("\r\n", "\n");
    let parsed: BTreeMap<String, String> = committed
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (k, v) = l.split_once(" = ").expect("name = value");
            (k.to_owned(), v.to_owned())
        })
        .collect();
    for (k, v) in &computed {
        assert_eq!(parsed.get(k), Some(v), "vector {k} differs");
    }
    assert_eq!(parsed.len(), computed.len(), "unexpected extra vectors");
}

#[test]
fn vectors_are_deterministic() {
    assert_eq!(compute(), compute());
}
