//! Bounded, deterministic properties over the public core API.
//!
//! These tests complement the fixed conformance vectors: vectors protect exact wire values, while
//! these properties exercise a wider input space without reaching into production internals.

use std::env;
use std::fmt::Debug;

use pigeonpost_core::envelope::{self, ENVELOPE_VERSION, MAX_TIMESTAMP_JITTER_SECS};
use pigeonpost_core::record::{ROTATION_CLOCK_SKEW_SECS, ROTATION_GRACE_SECS};
use pigeonpost_core::{
    pow, Address, Error, Identity, Result as CoreResult, RotationRecord, SuccessorCommitment, Wrap,
};
use proptest::collection;
use proptest::prelude::{any, prop_assert, prop_assert_eq, prop_assert_ne, Strategy};
use proptest::test_runner::{
    Config, RngAlgorithm, RngSeed, TestCaseError, TestCaseResult, TestRunner,
};

const SEED_ENV: &str = "PROPTEST_RNG_SEED";
const MAX_REJECTS: u32 = 64;
const MAX_SHRINK_ITERS: u32 = 256;
const MAX_BODY_CHARS: usize = 256;
const MAX_POW_ATTEMPTS: u16 = 512;

const ADDRESS_CASES: u32 = 128;
const ENVELOPE_CASES: u32 = 48;
const ROTATION_CASES: u32 = 64;
const POW_CASES: u32 = 128;

const ADDRESS_SEED: u64 = 0xa11d_d3e1_5eed_0001;
const ENVELOPE_SEED: u64 = 0xe11e_10e3_5eed_0002;
const ROTATION_SEED: u64 = 0xb07a_710a_5eed_0003;
const POW_SEED: u64 = 0x90a0_5eed_0000_0004;

fn configured_seed(fallback: u64) -> u64 {
    match env::var(SEED_ENV) {
        Ok(value) => value
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("{SEED_ENV} must be an unsigned 64-bit integer")),
        Err(env::VarError::NotPresent) => fallback,
        Err(env::VarError::NotUnicode(_)) => {
            panic!("{SEED_ENV} must contain Unicode decimal digits")
        }
    }
}

fn runner(cases: u32, fallback_seed: u64) -> TestRunner {
    TestRunner::new(Config {
        cases,
        max_local_rejects: MAX_REJECTS,
        max_global_rejects: MAX_REJECTS,
        max_flat_map_regens: MAX_REJECTS,
        failure_persistence: None,
        max_shrink_iters: MAX_SHRINK_ITERS,
        max_default_size_range: MAX_BODY_CHARS,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(configured_seed(fallback_seed)),
        ..Config::default()
    })
}

#[track_caller]
fn run_property<S>(
    cases: u32,
    fallback_seed: u64,
    strategy: S,
    test: impl Fn(S::Value) -> TestCaseResult,
) where
    S: Strategy,
    S::Value: Debug,
{
    if let Err(error) = runner(cases, fallback_seed).run(&strategy, test) {
        panic!("property failed: {error}");
    }
}

fn identity_for(mut seed: [u8; 32], domain: u8) -> Identity {
    // Replacing one byte gives every domain a provably distinct seed without rejecting a case.
    seed[31] = domain;
    Identity::from_seed(seed)
}

fn core_ok<T>(result: CoreResult<T>, context: &'static str) -> Result<T, TestCaseError> {
    result.map_err(|error| TestCaseError::fail(format!("{context}: {error}")))
}

fn assert_authenticated_tamper_rejected(
    recipient: &Identity,
    tampered: &Wrap,
    class: &'static str,
) -> TestCaseResult {
    prop_assert!(
        tampered.verify_public().is_err(),
        "{class} tamper passed public verification"
    );
    prop_assert!(
        envelope::open(recipient, tampered).is_err(),
        "{class} tamper opened successfully"
    );
    Ok(())
}

#[test]
fn addresses_derive_parse_match_and_separate_distinct_keys() {
    run_property(ADDRESS_CASES, ADDRESS_SEED, any::<[u8; 32]>(), |seed| {
        let first = identity_for(seed, 1);
        let second = identity_for(seed, 2);
        let first_key = first.verifying_key();
        let second_key = second.verifying_key();

        let address = first.address();
        prop_assert_eq!(address.clone(), Address::from_pubkey(&first_key));
        prop_assert_eq!(
            core_ok(Address::parse(address.as_str()), "parse derived address")?,
            address.clone()
        );
        prop_assert!(address.matches(&first_key));
        prop_assert!(!address.matches(&second_key));
        prop_assert_ne!(first_key, second_key);
        prop_assert_ne!(address, second.address());
        Ok(())
    });
}

#[test]
fn v3_envelopes_round_trip_and_authenticate_each_public_core_field() {
    let bodies = collection::vec(any::<char>(), 0..=MAX_BODY_CHARS)
        .prop_map(|characters| characters.into_iter().collect::<String>());
    let times = MAX_TIMESTAMP_JITTER_SECS..=u64::MAX;

    run_property(
        ENVELOPE_CASES,
        ENVELOPE_SEED,
        (any::<[u8; 32]>(), bodies, times),
        |(seed, body, now)| {
            let sender = identity_for(seed, 11);
            let recipient = identity_for(seed, 12);
            let wrong_recipient = identity_for(seed, 13);
            let unrelated_ephemeral = identity_for(seed, 14);

            let wrapped = core_ok(
                envelope::wrap(&sender, &recipient.verifying_key(), &body, now),
                "wrap v3 envelope",
            )?;
            prop_assert_eq!(wrapped.version, ENVELOPE_VERSION);
            prop_assert_eq!(wrapped.recipient, recipient.verifying_key().to_bytes());
            prop_assert!(wrapped.created_at <= now);
            prop_assert!(wrapped.created_at >= now - MAX_TIMESTAMP_JITTER_SECS);
            core_ok(wrapped.verify_public(), "verify v3 public envelope")?;

            let (opened_sender, opened_body) =
                core_ok(envelope::open(&recipient, &wrapped), "open v3 envelope")?;
            prop_assert_eq!(opened_sender, sender.verifying_key());
            prop_assert_eq!(opened_body.as_str(), body.as_str());
            prop_assert!(envelope::open(&wrong_recipient, &wrapped).is_err());

            // These are the six v3 outer-core fields covered by the ephemeral signature. The PoW
            // nonce is deliberately absent: it is mined after signing and is verified separately.
            let mut bad_version = wrapped.clone();
            bad_version.version ^= 1;
            assert_authenticated_tamper_rejected(&recipient, &bad_version, "version")?;

            let mut bad_ephemeral = wrapped.clone();
            bad_ephemeral.ephemeral_pubkey = unrelated_ephemeral.verifying_key().to_bytes();
            assert_authenticated_tamper_rejected(
                &recipient,
                &bad_ephemeral,
                "ephemeral public key",
            )?;

            let mut bad_recipient = wrapped.clone();
            bad_recipient.recipient = wrong_recipient.verifying_key().to_bytes();
            assert_authenticated_tamper_rejected(&recipient, &bad_recipient, "recipient")?;

            let mut bad_nonce = wrapped.clone();
            bad_nonce.nonce[0] ^= 1;
            assert_authenticated_tamper_rejected(&recipient, &bad_nonce, "nonce")?;

            let mut bad_ciphertext = wrapped.clone();
            bad_ciphertext.ciphertext[0] ^= 1;
            assert_authenticated_tamper_rejected(&recipient, &bad_ciphertext, "ciphertext")?;

            let mut bad_time = wrapped.clone();
            bad_time.created_at ^= 1;
            assert_authenticated_tamper_rejected(&recipient, &bad_time, "created_at")?;

            let mut bad_signature = wrapped;
            bad_signature.signature[0] ^= 1;
            assert_authenticated_tamper_rejected(&recipient, &bad_signature, "signature")?;
            Ok(())
        },
    );
}

#[test]
fn dual_signed_rotations_enforce_chain_sequence_keys_and_time() {
    const MIN_ACTIVATION: u64 = ROTATION_CLOCK_SKEW_SECS + 2;
    const MAX_ACTIVATION_EXCLUSIVE: u64 = u64::MAX - ROTATION_GRACE_SECS;

    run_property(
        ROTATION_CASES,
        ROTATION_SEED,
        (
            any::<[u8; 32]>(),
            any::<u64>(),
            MIN_ACTIVATION..MAX_ACTIVATION_EXCLUSIVE,
        ),
        |(seed, raw_seq, activated_at)| {
            let outgoing = identity_for(seed, 21);
            let incoming = identity_for(seed, 22);
            let next = identity_for(seed, 23);
            let attacker = identity_for(seed, 24);
            let seq = if raw_seq == 0 { 1 } else { raw_seq };
            let pinned = SuccessorCommitment::for_key(&incoming.verifying_key());
            let next_commitment = SuccessorCommitment::for_key(&next.verifying_key());

            let record = core_ok(
                RotationRecord::new(&outgoing, &incoming, &next_commitment, seq, activated_at),
                "construct rotation",
            )?;
            prop_assert_eq!(record.seq, seq);
            prop_assert_eq!(record.activated_at, activated_at);
            prop_assert_eq!(record.grace_until, activated_at + ROTATION_GRACE_SECS);
            core_ok(
                record.verify_source_address(&outgoing.address()),
                "verify source address",
            )?;
            prop_assert_eq!(
                core_ok(record.target_address(), "derive target address")?,
                incoming.address()
            );

            let verified = core_ok(
                record.verify(&pinned, seq - 1, activated_at),
                "verify rotation",
            )?;
            prop_assert_eq!(verified.incoming, incoming.verifying_key());
            prop_assert_eq!(verified.next_successor, next_commitment);
            prop_assert_eq!(verified.activated_at, activated_at);
            prop_assert_eq!(verified.grace_until, record.grace_until);
            prop_assert!(!record.retired_key_is_active(activated_at - 1));
            prop_assert!(record.retired_key_is_active(activated_at));
            prop_assert!(record.retired_key_is_active(record.grace_until - 1));
            prop_assert!(!record.retired_key_is_active(record.grace_until));

            prop_assert!(matches!(
                record.verify_source_address(&attacker.address()),
                Err(Error::MalformedAddress(_))
            ));
            let attacker_pin = SuccessorCommitment::for_key(&attacker.verifying_key());
            prop_assert_eq!(
                record.verify(&attacker_pin, seq - 1, activated_at),
                Err(Error::SuccessorMismatch)
            );
            prop_assert_eq!(
                record.verify(&pinned, seq, activated_at),
                Err(Error::StaleSequence)
            );
            let too_early = activated_at - ROTATION_CLOCK_SKEW_SECS - 1;
            prop_assert_eq!(
                record.verify(&pinned, seq - 1, too_early),
                Err(Error::StaleTimestamp)
            );

            let mut bad_outgoing_signature = record.clone();
            bad_outgoing_signature.outgoing_signature[0] ^= 1;
            prop_assert_eq!(
                bad_outgoing_signature.verify(&pinned, seq - 1, activated_at),
                Err(Error::BadSignature)
            );

            let mut bad_incoming_signature = record.clone();
            bad_incoming_signature.incoming_signature[0] ^= 1;
            prop_assert_eq!(
                bad_incoming_signature.verify(&pinned, seq - 1, activated_at),
                Err(Error::BadSignature)
            );

            let mut bad_timing = record.clone();
            bad_timing.grace_until -= 1;
            prop_assert!(matches!(
                bad_timing.verify(&pinned, seq - 1, activated_at),
                Err(Error::MalformedEnvelope(_))
            ));

            let mut bad_next_hash = record;
            bad_next_hash.next_successor_hash[0] ^= 1;
            prop_assert_eq!(
                bad_next_hash.verify(&pinned, seq - 1, activated_at),
                Err(Error::BadSignature)
            );
            Ok(())
        },
    );
}

#[test]
fn proof_of_work_verifies_exact_work_and_mines_only_within_the_bound() {
    run_property(
        POW_CASES,
        POW_SEED,
        (
            any::<[u8; 32]>(),
            any::<u64>(),
            0u32..=12,
            0u16..=MAX_POW_ATTEMPTS,
        ),
        |(id, nonce, difficulty, max_attempts)| {
            let measured = pow::work(&id, nonce);
            prop_assert!(pow::verify(&id, nonce, measured).is_ok());
            prop_assert_eq!(
                pow::verify(&id, nonce, measured + 1),
                Err(Error::InsufficientWork)
            );
            prop_assert!(pow::verify(&id, nonce, 0).is_ok());

            let max_attempts = u64::from(max_attempts);
            let expected =
                (0..max_attempts).find(|candidate| pow::work(&id, *candidate) >= difficulty);
            let mined = pow::mine(&id, difficulty, max_attempts);
            prop_assert_eq!(mined, expected);

            if let Some(first) = mined {
                prop_assert!(pow::verify(&id, first, difficulty).is_ok());
                let first_is_minimal =
                    (0..first).all(|candidate| pow::work(&id, candidate) < difficulty);
                prop_assert!(first_is_minimal);
            } else {
                let search_space_is_exhausted =
                    (0..max_attempts).all(|candidate| pow::work(&id, candidate) < difficulty);
                prop_assert!(search_space_is_exhausted);
            }
            Ok(())
        },
    );
}
