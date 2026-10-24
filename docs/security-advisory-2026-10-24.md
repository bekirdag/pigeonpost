# Dependency security review — 2026-10-24

The continuation patches compatible transport and cryptographic-helper dependencies rather
than adding exceptions for them: quinn-proto 0.11.19, rustls 0.23.46, rustls-webpki 0.103.16,
webpki-roots 1.0.11, ctutils 0.4.5, Tokio 1.54.1 and the non-yanked chacha20 0.10.2. The group
crypto workspace has its own lockfile; its ctutils version is patched separately.

## crypto-bigint 0.5.5: current reachability assessment

[RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html) reports timing
variability in integer operations intended to be constant-time. It can leak secrets when affected
operations process secret integers. The patched line is 0.7.6 or newer. OpenMLS's currently
published `openmls_rust_crypto` 0.6.0, `openmls_basic_credential` 0.6.0 and HPKE dependencies still
require the older curve stack. A compatible lockfile update cannot replace crypto-bigint 0.5.5.

Current Pigeonpost production secret paths do not use those affected integer operations:

| Path | Evidence and boundary |
|---|---|
| Group creation, signing, HPKE and messages | `crates/pigeonpost-groupcrypto/src/device.rs` fixes the sole suite to `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (0x0001). It generates only Ed25519 signing keys and X25519 HPKE keys; it does not negotiate P-256/P-384/K-256. |
| Incoming Welcome | The wrapper now rejects every other suite before calling OpenMLS or accessing key-package secrets. The previous OpenMLS implementation also refused suite mismatches before HPKE decryption, but consumed a matching key package first. The new check prevents that input-driven key-package loss. |
| Imported group state | `src/state.rs` requires Ed25519 and a 32-byte public signing key. State is authenticated and encrypted under the device's local state key. A party holding that key already has the encrypted ratchet state; untrusted group traffic cannot substitute it. |
| Direct calendar guest proof | `crates/pigeonpost-postbox/src/calendar/guest.rs` uses P-256 for signature verification with public keys, signatures and challenge bytes. Private-key generation/signing appears only in `calendar/tests.rs`. No P-256 signing or ECDH secret is handled in this production path. |

The exception in both audit configurations records **non-applicability to these current secret
paths**, not a patched crypto-bigint release. It must not be reused as a blanket assessment of the
RustCrypto provider's other algorithms. There is no vendored cryptographic patch or provider
migration in this change. Existing optional, uncompiled libcrux exceptions remain separate.

## Evidence and limits

- The inverse Cargo tree identifies the old integer/curve stack through the OpenMLS providers,
  HPKE and the direct calendar verifier. Root and standalone group lockfiles are reviewed separately.
- `unsupported_welcome_suite_is_rejected_without_consuming_the_key_package` uses a genuine
  X25519 Welcome, changes its advertised suite to P-256, requires an immediate unsupported-suite
  error, and then joins successfully with the original Welcome on the same device. It also pins
  the expected wire header to suite 0x0001. The test failed against the prior implementation.
- Standalone group lifecycle, state, history-transfer and hostile-input property tests, plus
  Clippy, pass after the boundary change. Cross-language/native qualification remains part of the
  release batch; these local passes do not replace the planned independent group-crypto review.

Reassess this exception before enabling any new cipher suite, P-curve signing/ECDH, SSH key
algorithm, imported-key path or provider. Review again when the upstream provider changes and
remove it as soon as the affected crypto-bigint dependency can be upgraded away. An advisory
remaining in the lockfile must remain visible in release review notes even when `cargo audit`
passes with this documented exception.
