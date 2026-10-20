//! End-to-end scenarios against a minimal in-memory delivery service that enforces the
//! server rules from `docs/group-e2ee.md`: one commit per epoch (first valid wins), epoch-bound
//! application messages, one global sequence per group, welcome routing by KeyPackageRef.

use pigeonpost_groupcrypto::*;

fn seed(n: u8) -> [u8; 32] {
    [n; 32]
}

fn device(n: u8, id: &str) -> Device {
    Device::new_with_identity_seed(&seed(n), id).unwrap()
}

#[derive(Default)]
struct Server {
    epoch: u64,
    log: Vec<(Vec<u8>, Vec<u8>)>,      // (sender device key, message)
    welcomes: Vec<(Vec<u8>, Vec<u8>)>, // (kp ref, welcome)
    seen_ids: std::collections::BTreeSet<(String, String)>,
}

#[derive(Debug, PartialEq)]
enum Reject {
    EpochConflict,
    Duplicate,
}

impl Server {
    fn post_commit(
        &mut self,
        b: &CommitBundle,
        signer_key: &[u8],
    ) -> std::result::Result<u64, Reject> {
        verify_envelope_signature(signer_key, &b.commit, &b.envelope_signature).unwrap();
        let info = inspect_envelope(&b.commit).unwrap();
        assert_eq!(info.kind, MessageKind::Commit);
        if !self
            .seen_ids
            .insert((info.sender_mailbox.clone(), info.client_message_id.clone()))
        {
            return Err(Reject::Duplicate);
        }
        if info.epoch != self.epoch {
            return Err(Reject::EpochConflict);
        }
        self.epoch += 1;
        let seq = self.log.len() as u64;
        self.log.push((signer_key.to_vec(), b.commit.clone()));
        if let Some(w) = &b.welcome {
            for r in welcome_recipients(w).unwrap() {
                self.welcomes.push((r, w.clone()));
            }
        }
        Ok(seq)
    }

    fn post_message(
        &mut self,
        m: &OutgoingMessage,
        signer_key: &[u8],
    ) -> std::result::Result<u64, Reject> {
        verify_envelope_signature(signer_key, &m.ciphertext, &m.envelope_signature).unwrap();
        let info = inspect_envelope(&m.ciphertext).unwrap();
        assert_eq!(info.kind, MessageKind::Application);
        if info.epoch != self.epoch {
            return Err(Reject::EpochConflict);
        }
        if !self
            .seen_ids
            .insert((info.sender_mailbox.clone(), info.client_message_id.clone()))
        {
            return Err(Reject::Duplicate);
        }
        let seq = self.log.len() as u64;
        self.log.push((signer_key.to_vec(), m.ciphertext.clone()));
        Ok(seq)
    }

    fn welcome_for(&self, kp: &[u8]) -> Vec<u8> {
        let r = inspect_key_package(kp).unwrap().key_package_ref;
        self.welcomes
            .iter()
            .find(|(x, _)| *x == r)
            .unwrap()
            .1
            .clone()
    }
}

/// A client cursor: processes server log entries after `next`.
struct Client {
    d: Device,
    next: u64,
    inbox: Vec<ReceivedMessage>,
}

impl Client {
    fn new(d: Device) -> Self {
        Self {
            d,
            next: 0,
            inbox: vec![],
        }
    }
    fn sync(&mut self, s: &Server, g: &str) {
        while (self.next as usize) < s.log.len() {
            let (from, msg) = &s.log[self.next as usize];
            self.next += 1;
            if *from == self.key() {
                continue; // the postbox does not echo a device's own posts back to it
            }
            match self.d.process_incoming(g, msg).unwrap() {
                Incoming::Message(m) => self.inbox.push(m),
                Incoming::Own | Incoming::Commit(_) | Incoming::OwnCommitConfirmed(_) => {}
                Incoming::HistoryOffer(_) => {}
            }
        }
    }
    fn key(&self) -> Vec<u8> {
        self.d.signature_public_key()
    }
}

const G: &str = "grp_0123456789abcdefghjkmnpqrs";

#[test]
fn full_lifecycle() {
    let mut s = Server::default();
    let mut alice = Client::new(device(1, "phone"));
    let mut bob = Client::new(device(2, "laptop"));
    let mut carol = Client::new(device(3, "desktop"));

    let bob_kp = bob.d.generate_key_packages(2).unwrap();
    let carol_kp = carol.d.generate_key_packages(1).unwrap();
    let info = inspect_key_package(&bob_kp[0]).unwrap();
    assert_eq!(info.mailbox, bob.d.mailbox());
    assert_eq!(info.device_id, "laptop");
    assert!(!info.last_resort);

    // Creation and activation: alice creates epoch 0 and commits the initial membership.
    alice.d.create_group(G).unwrap();
    let c = alice
        .d
        .add_members(G, &[bob_kp[0].clone(), carol_kp[0].clone()])
        .unwrap();
    assert_eq!(c.welcome_recipients.len(), 2);
    let seq = s.post_commit(&c, &alice.key()).unwrap();
    alice.d.confirm_commit(G).unwrap();
    alice.next = seq + 1; // the sender does not need its own commit back
    for c in [&mut bob, &mut carol] {
        let w = s.welcome_for(if c.d.device_id() == "laptop" {
            &bob_kp[0]
        } else {
            &carol_kp[0]
        });
        let joined = c.d.join_from_welcome(&w).unwrap();
        assert_eq!(joined.epoch, 1);
        assert_eq!(joined.members.len(), 3);
        c.next = seq + 1;
    }
    assert_eq!(
        alice.d.safety_number(G).unwrap(),
        bob.d.safety_number(G).unwrap()
    );
    assert_eq!(
        alice.d.epoch_authenticator(G).unwrap(),
        carol.d.epoch_authenticator(G).unwrap()
    );

    // Messages with an attachment.
    let sealed = seal_attachment(b"holiday.jpg bytes").unwrap();
    let m = alice
        .d
        .encrypt_message(
            G,
            "m-1",
            br#"{"text":"hi"}"#,
            std::slice::from_ref(&sealed.reference),
        )
        .unwrap();
    s.post_message(&m, &alice.key()).unwrap();
    bob.sync(&s, G);
    carol.sync(&s, G);
    let got = &bob.inbox[0];
    assert_eq!(got.sender.mailbox, alice.d.mailbox());
    assert_eq!(got.sender.device_id, "phone");
    assert_eq!(got.client_message_id, "m-1");
    assert_eq!(got.body, br#"{"text":"hi"}"#);
    assert_eq!(
        open_attachment(&sealed.sealed, &got.attachments[0]).unwrap(),
        b"holiday.jpg bytes"
    );
    // A replayed ciphertext is rejected (keys are deleted after first use).
    assert!(bob.d.process_incoming(G, &m.ciphertext).is_err());

    // Removal: carol is removed by bob; carol processes the commit and loses state.
    let carol_key = generate_state_key().unwrap();
    let carol_backup = carol.d.export_state(&carol_key).unwrap();
    let c = bob.d.remove_mailbox(G, carol.d.mailbox()).unwrap();
    s.post_commit(&c, &bob.key()).unwrap();
    bob.d.confirm_commit(G).unwrap();
    alice.sync(&s, G);
    let removal = carol
        .d
        .process_incoming(G, &s.log.last().unwrap().1)
        .unwrap();
    match removal {
        Incoming::Commit(o) => assert!(o.self_removed),
        other => panic!("{other:?}"),
    }
    carol.next += 1;
    assert_eq!(carol.d.process_incoming(G, b"x"), Err(Error::UnknownGroup));
    let m = alice
        .d
        .encrypt_message(G, "m-2", b"after removal", &[])
        .unwrap();
    s.post_message(&m, &alice.key()).unwrap();
    bob.sync(&s, G);
    assert_eq!(bob.inbox.last().unwrap().body, b"after removal");
    // Even with a copy of her pre-removal state, carol cannot read post-removal traffic:
    // skipping the removal commit leaves her an epoch behind...
    let mut carol_old = Device::import_state(&carol_backup, &carol_key).unwrap();
    assert!(matches!(
        carol_old.process_incoming(G, &m.ciphertext),
        Err(Error::FutureEpoch { .. })
    ));
    // ...and processing it only tells her she was removed.
    let removal_commit = s.log[s.log.len() - 2].1.clone();
    match carol_old.process_incoming(G, &removal_commit).unwrap() {
        Incoming::Commit(o) => assert!(o.self_removed),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        carol_old.process_incoming(G, &m.ciphertext).unwrap_err(),
        Error::UnknownGroup
    );
    assert_eq!(alice.d.members(G).unwrap().len(), 2);

    // Concurrent commits: alice and bob both self-update at the same epoch. Bob wins.
    let a = alice.d.self_update(G).unwrap();
    let b = bob.d.self_update(G).unwrap();
    s.post_commit(&b, &bob.key()).unwrap();
    bob.d.confirm_commit(G).unwrap();
    assert_eq!(s.post_commit(&a, &alice.key()), Err(Reject::EpochConflict));
    alice.d.discard_commit(G).unwrap();
    alice.sync(&s, G);
    let a2 = alice.d.self_update(G).unwrap();
    s.post_commit(&a2, &alice.key()).unwrap();
    alice.d.confirm_commit(G).unwrap();
    bob.sync(&s, G);
    assert_eq!(alice.d.epoch(G).unwrap(), bob.d.epoch(G).unwrap());
    assert_eq!(
        alice.d.epoch_authenticator(G).unwrap(),
        bob.d.epoch_authenticator(G).unwrap()
    );

    // A message encrypted in an old epoch is refused by the server and must be re-encrypted.
    let stale = bob.d.encrypt_message(G, "m-3", b"late", &[]).unwrap();
    let c = alice.d.self_update(G).unwrap();
    s.post_commit(&c, &alice.key()).unwrap();
    alice.d.confirm_commit(G).unwrap();
    assert_eq!(
        s.post_message(&stale, &bob.key()),
        Err(Reject::EpochConflict)
    );
    bob.sync(&s, G);
    let fresh = bob.d.encrypt_message(G, "m-3", b"late", &[]).unwrap();
    s.post_message(&fresh, &bob.key()).unwrap();
    alice.sync(&s, G);
    assert_eq!(alice.inbox.last().unwrap().body, b"late");
    // And a stale ciphertext delivered anyway is rejected by clients.
    assert!(matches!(
        alice.d.process_incoming(G, &stale.ciphertext),
        Err(Error::StaleEpoch { .. })
    ));

    // New device for bob (same mailbox): no history, joins at the current epoch.
    let mut bob2 = Client::new(device(2, "phone"));
    let kp = bob2.d.generate_last_resort_key_package().unwrap();
    assert!(inspect_key_package(&kp).unwrap().last_resort);
    let c = bob.d.add_members(G, std::slice::from_ref(&kp)).unwrap();
    let seq = s.post_commit(&c, &bob.key()).unwrap();
    bob.d.confirm_commit(G).unwrap();
    alice.sync(&s, G);
    bob2.d.join_from_welcome(&s.welcome_for(&kp)).unwrap();
    bob2.next = seq + 1;
    assert!(bob2.inbox.is_empty());
    let m = alice
        .d
        .encrypt_message(G, "m-4", b"welcome bob2", &[])
        .unwrap();
    s.post_message(&m, &alice.key()).unwrap();
    bob2.sync(&s, G);
    bob.sync(&s, G);
    assert_eq!(bob2.inbox[0].body, b"welcome bob2");

    // Persistence: export, import, keep going.
    let key = generate_state_key().unwrap();
    let blob = alice.d.export_state(&key).unwrap();
    assert!(Device::import_state(&blob, &[0u8; 32]).is_err());
    alice.d = Device::import_state(&blob, &key).unwrap();
    let m = alice
        .d
        .encrypt_message(G, "m-5", b"after restore", &[])
        .unwrap();
    s.post_message(&m, &alice.key()).unwrap();
    bob.sync(&s, G);
    assert_eq!(bob.inbox.last().unwrap().body, b"after restore");
}

#[test]
fn own_commit_echo_confirms() {
    let mut s = Server::default();
    let mut alice = Client::new(device(1, "a"));
    let mut bob = Client::new(device(2, "b"));
    let kp = bob.d.generate_key_packages(1).unwrap();
    alice.d.create_group(G).unwrap();
    let c = alice.d.add_members(G, &kp).unwrap();
    s.post_commit(&c, &alice.key()).unwrap();
    // Alice "crashes" before confirming; on restart she sees her own commit in the log.
    match alice.d.process_incoming(G, &s.log[0].1).unwrap() {
        Incoming::OwnCommitConfirmed(o) => {
            assert_eq!(o.epoch, 1);
            assert_eq!(o.commit_id, c.commit_id);
            assert_eq!(o.added.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    bob.d.join_from_welcome(&s.welcome_for(&kp[0])).unwrap();
    assert_eq!(
        alice.d.epoch_authenticator(G).unwrap(),
        bob.d.epoch_authenticator(G).unwrap()
    );
}

#[test]
fn history_transfer_only_between_own_devices() {
    let mut s = Server::default();
    let mut old = Client::new(device(7, "old"));
    let mut new = Client::new(device(7, "new"));
    let kp = new.d.generate_key_packages(1).unwrap();
    // Fingerprint comparison happens in the UI before this add.
    assert_eq!(fingerprint_digits(&new.d.fingerprint()).len(), 35);
    old.d.create_group("self_7").unwrap();
    let c = old.d.add_members("self_7", &kp).unwrap();
    s.post_commit(&c, &old.key()).unwrap();
    old.d.confirm_commit("self_7").unwrap();
    old.next = 1;
    new.d.join_from_welcome(&s.welcome_for(&kp[0])).unwrap();
    new.next = 1;
    let t = old
        .d
        .seal_history("self_7", "h-1", b"archived conversation")
        .unwrap();
    s.post_message(&t.message, &old.key()).unwrap();
    let offer = match new.d.process_incoming("self_7", &s.log[1].1).unwrap() {
        Incoming::HistoryOffer(o) => o,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        new.d.open_history(&offer, &t.sealed_archive).unwrap(),
        b"archived conversation"
    );

    // In a group with another mailbox, history transfer is refused.
    let mut other = device(8, "x");
    let kp = other.generate_key_packages(1).unwrap();
    old.d.create_group("mixed").unwrap();
    old.d.add_members("mixed", &kp).unwrap();
    old.d.confirm_commit("mixed").unwrap();
    assert_eq!(
        old.d.seal_history("mixed", "h-2", b"x").unwrap_err(),
        Error::NotSelfGroup
    );
}

#[test]
fn rejects_bad_inputs() {
    assert!(Device::new("/k/notanaddress", "d").is_err());
    let mut d = Device::new(&mailbox_for_seed(&seed(1)), "dev").unwrap();
    assert_eq!(d.generate_key_packages(1).unwrap_err(), Error::NotAttested);
    // An attestation by the wrong mailbox key is refused.
    let payload = d.attestation_payload().unwrap();
    let sig = sign_attestation_with_seed(&seed(2), &payload).unwrap();
    let wrong_pk = ed25519_dalek_pk(&seed(2));
    assert!(matches!(
        d.install_attestation(&wrong_pk, &sig),
        Err(Error::InvalidCredential(_))
    ));
    let sig = sign_attestation_with_seed(&seed(1), &payload).unwrap();
    d.install_attestation(&ed25519_dalek_pk(&seed(1)), &sig)
        .unwrap();
    assert!(d.is_attested());

    // Duplicate device in a commit.
    let mut bob = device(2, "b");
    let kps = bob.generate_key_packages(2).unwrap();
    d.create_group(G).unwrap();
    assert!(matches!(
        d.add_members(G, &kps),
        Err(Error::DuplicateDevice(_))
    ));
    assert_eq!(d.create_group(G).unwrap_err(), Error::GroupExists);
    assert!(d.encrypt_message(G, "bad id!", b"", &[]).is_err());
    assert_eq!(d.discard_commit(G).unwrap_err(), Error::NoPendingCommit);
    d.add_members(G, &kps[..1]).unwrap();
    assert_eq!(d.self_update(G).unwrap_err(), Error::PendingCommit);
}

#[test]
fn tampering_is_detected() {
    let mut alice = device(1, "a");
    let mut bob = device(2, "b");
    let kp = bob.generate_key_packages(1).unwrap();
    alice.create_group(G).unwrap();
    let c = alice.add_members(G, &kp).unwrap();
    alice.confirm_commit(G).unwrap();
    bob.join_from_welcome(c.welcome.as_ref().unwrap()).unwrap();
    let m = alice.encrypt_message(G, "m-1", b"secret", &[]).unwrap();
    // Flip a bit in the ciphertext body.
    let mut bad = m.ciphertext.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(bob.process_incoming(G, &bad).is_err());
    // The envelope signature does not verify for modified bytes or the wrong device.
    assert!(
        verify_envelope_signature(&alice.signature_public_key(), &bad, &m.envelope_signature)
            .is_err()
    );
    assert!(verify_envelope_signature(
        &bob.signature_public_key(),
        &m.ciphertext,
        &m.envelope_signature
    )
    .is_err());
    // The untouched message still decrypts (the failed attempt consumed nothing valid).
    match bob.process_incoming(G, &m.ciphertext).unwrap() {
        Incoming::Message(r) => assert_eq!(r.body, b"secret"),
        other => panic!("{other:?}"),
    }
    // Attachments: wrong blob or flipped bit fail.
    let sealed = seal_attachment(b"blob").unwrap();
    let mut blob = sealed.sealed.clone();
    blob[30] ^= 1;
    assert_eq!(
        open_attachment(&blob, &sealed.reference).unwrap_err(),
        Error::DecryptionFailed
    );
}

fn ed25519_dalek_pk(seed: &[u8; 32]) -> Vec<u8> {
    ed25519_dalek::SigningKey::from_bytes(seed)
        .verifying_key()
        .to_bytes()
        .to_vec()
}
