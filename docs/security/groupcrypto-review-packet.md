# External review packet: `pigeonpost-groupcrypto` (group end-to-end encryption)

Prepared 2026-10-28 for an independent cryptographic and code review before groups reach GA
(SDS §23, release prerequisite P-2). Nobody outside the project has been contacted yet. The owner
chooses the reviewer and shares this packet.

## 1. Scope

**Review baseline:** the `main` commit that merges this packet. The crate's `src/` last changed in `374959f`,
which rejects unsupported Welcome suites before key-package access. This packet adds only the
vector test and its checker. Record the exact commit
hash you review in your report.

### In scope

| Area | Files | Why |
|---|---|---|
| Crate source (≈2,300 lines of Rust, `unsafe_code = "forbid"`) | `crates/pigeonpost-groupcrypto/src/{lib,device,credential,payload,attachment,state,codec,random,error}.rs` | Everything Pigeonpost adds on top of OpenMLS. |
| Protocol specification | `docs/group-e2ee.md` §2–§10, §12–§13 | The contract that clients and the server implement. |
| OpenMLS configuration and usage | `src/device.rs` (`create_config`, `join_config`, `commit`, `join_from_welcome`, `process_incoming`, `summarize`) | Profile choices: suite 0x0001 only, `PURE_CIPHERTEXT`, `max_past_epochs = 0`, no standalone proposals, no external commits or PSKs, ratchet tree in the Welcome, last-resort key packages. |
| Server-side validation | `crates/pigeonpost-postbox/src/groups.rs` (uses `inspect_envelope`, `verify_envelope_signature`, `inspect_key_package`, `welcome_recipients`), `crates/pigeonpost-postbox/src/mls.rs` (device, key-package, handshake and Welcome routes) | What the delivery service checks before storing ciphertext, and the commit-serialization transaction (`docs/group-e2ee.md` §7.3). |
| Binding layer | `ffi/src/lib.rs` (UniFFI: Swift, Kotlin, Python, C#), `wasm/src/lib.rs` (wasm-bindgen) | Key material crossing FFI boundaries; error mapping; state export/import. |
| Web loader | `site-inbox/vendor/groupcrypto/{loader.js,integrity.js,SHA256SUMS}` and `site-inbox/groups-core.js` (state-key wrapping in IndexedDB) | Supply chain and key storage in the browser. |

### Out of scope

- OpenMLS 0.9.1 and its RustCrypto provider as libraries. Report anything you notice, but this
  packet does not commission a full OpenMLS audit.
- Native application UI (iOS, Android, Windows, Linux): they consume the bindings. UI duties that
  carry security weight are listed in §4 as questions.
- The custodial direct-message path, calls, live streams and meeting summaries. These have
  separate designs (`docs/calls-sfu.md`, `docs/streaming.md`, `docs/meeting-summaries.md`).
- Infrastructure (hosts, TLS termination). The SSH broker has its own scope:
  `docs/security/agent-ssh-pentest-scope.md`.

## 2. Architecture summary

- **MLS (RFC 9420)** through OpenMLS `=0.9.1`, with only the mandatory-to-implement suite
  `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`. Group key agreement, message protection, forward
  secrecy and post-compromise security all come from MLS.
- **Device credentials** (`src/credential.rs`). Each leaf's `BasicCredential` identity is
  `u8 version || u16 len || /k/ mailbox || u16 len || device id || mailbox ed25519 public key ||
  attestation signature`. The mailbox identity key signs
  `"pigeonpost/groupcrypto/device-attestation/v1\0" || mailbox || device id || suite || leaf
  signature key`. A `/k/` address is the Crockford base32 of SHA-256(public key)[..16], so no
  directory is needed to check the binding. `verify_strict` is used throughout.
- **Message AAD** (`src/payload.rs`). Every `PrivateMessage` carries
  `"PPGC" || 1 || kind || group id || sender mailbox || sender device || epoch || client message
  id`. It is cross-checked against the MLS header on the server (`inspect_envelope`) and against
  the decrypted sender's verified credential on recipients (`process_incoming`).
- **Envelope signature.** The device's MLS signature key also signs
  `"pigeonpost/groupcrypto/envelope/v1\0" || SHA-256(message)`, so the server can authenticate the
  uploading device without decrypting. This is deliberate, domain-separated key reuse
  (`docs/group-e2ee.md` §13.4).
- **Payload.** The encrypted payload is versioned. It holds the body (≤ 1 MiB) and up to 32
  attachment references (key, sealed hash, length).
- **Attachments** (`src/attachment.rs`). Each blob is sealed under a fresh 256-bit key with
  XChaCha20-Poly1305 and a random 24-byte nonce. The AAD is the 5-byte header `"PPGA" || 1`. The
  server addresses blobs by SHA-256 of the sealed bytes, and recipients check that hash before
  decrypting.
- **State at rest** (`src/state.rs`). The device's whole state, OpenMLS storage included, is one
  XChaCha20-Poly1305 blob under a caller key held in the platform keystore. Callers must overwrite
  it after every mutating call.
- **History.** New devices receive no history. Transfer is possible only between one mailbox's
  own devices, inside a *self group* (`seal_history`/`open_history`). The crate enforces that rule
  both when sending and when receiving.

## 3. Threat model (summary; the full table is `docs/group-e2ee.md` §12)

**What the server (postbox / delivery service) can do:**
- see metadata: membership, devices, sender, epoch, size padded to 32 bytes, timing, attachment
  sizes;
- delay, drop or reorder messages, refuse delivery, or keep ciphertext;
- fork a group by sending different commit streams to different members (detectable by comparing
  `epoch_authenticator` or safety numbers);
- accept a membership commit from a member that the group's roles do not allow, because **roles
  and posting rights are server-enforced, not cryptographic**. Clients show the verified committer
  of every change.

**What the server cannot do (claimed):**
- read application messages, attachments or history archives;
- forge a message or commit from a member device;
- make a removed device decrypt later epochs;
- make an honest client accept a leaf whose credential is not attested by its mailbox key;
- add a device to a *self-custodied* mailbox (CLI, agentd).

**Custodial identity keys (the main structural risk).** For *hosted* mailboxes, the postbox holds
the mailbox identity key, so it can mint a validly attested device and add it to that mailbox's
groups. Nothing prevents this; it is only detectable:
- `group.device` events and `device_added` notices;
- per-member fingerprints and the group safety number, which every client must show;
- clients remember verified fingerprints and flag changes.

A second device for a hosted mailbox requires a person's session. Since Phase 5 production also
requires step-up (`acr` allowlist `mfa`); `docs/group-e2ee.md` §16 item 1 now says so (updated in the
Phase 9 follow-up). `device_fingerprint` no longer truncates lengths over `u16::MAX` into an ambiguous
prefix: such inputs hash under a separate domain with `u64` lengths (valid inputs and the vectors are
unchanged).
Any credential may enrol a mailbox's *first* device (agents need this). Co-members can see that
device.

## 4. Questions for the reviewers

1. **Credential binding.** Are `DeviceCredential::verify` and its use in `member_info`,
   `join_from_welcome` and `summarize` sufficient? In particular:
   - every added, updated and committer-path leaf must be verified;
   - updated leaves must keep the same identity and signature key;
   - duplicate `(mailbox, device)` leaves must be refused.
   Can a commit introduce an unverified leaf by any path, such as an update path or a key package
   with extensions?
2. **AAD and header binding.** `inspect_envelope` (server) and `process_incoming` (client) both
   compare the AAD against the MLS framing. Is there a mismatch an attacker can exploit, for
   example between the AAD's sender device and the actual leaf index after a remove/add in the
   same epoch?
3. **Envelope-signature key reuse.** Is reusing the MLS leaf signature key for a domain-separated
   envelope signature safe against cross-protocol confusion with MLS `SignContent`/`LeafNodeTBS`
   encodings?
4. **Profile choices.** Do `max_past_epochs = 0` and the strict epoch equality in
   `process_incoming` (`StaleEpoch`/`FutureEpoch`) create any availability or security problem? Is
   the server's "first valid commit wins" serialization sound when two members commit in the same
   epoch?
5. **Last-resort key packages.** They can be consumed more than once. Is the mitigation adequate:
   self-update after a last-resort join, and keep topping up single-use packages?
6. **State blob.** The format and import validation in `src/state.rs`; zeroization of the signer
   and plaintext; the claim that callers must overwrite old blobs. Can a hostile blob (with a valid
   key) cause harm on import beyond memory use bounded by its length?
7. **Attachment format.** A random 192-bit XChaCha nonce under a fresh key per blob; hash-then-open
   ordering; the length check after decryption.
8. **Safety number and fingerprint.**
   - Fingerprint digits use 5 bytes mod 100000 per group (slight bias, 6 groups).
   - The safety number is SHA-512 over the sorted, de-duplicated fingerprints; 60 bytes give 12
     groups.
   Is this adequate for a human comparison against a server that can grind device keys (≈100 bits
   for the 6-group form)?
9. **Web key storage.** The state key is wrapped by a non-extractable WebCrypto key in IndexedDB.
   The barrier is the inbox CSP (`script-src 'self' 'wasm-unsafe-eval'`). Is this acceptable, and
   what else should constrain same-origin script?
10. **Bindings.**
    - Do the UniFFI and wasm-bindgen layers leak secrets in debug output, error strings or
      panics?
    - Is `panic = "unwind"` across FFI handled?
    - Are byte buffers containing keys zeroized where the host language allows?
11. **Replay and duplicates.** MLS deletes message keys after use; clients deduplicate on
    `(sender mailbox, client_message_id)`. Is there a replay window across the server's sequence
    and an epoch change?

## 5. Known limitations and open risks (from the implementation ledger)

- No published independent audit of OpenMLS as a library. Several OpenMLS advisories were fixed
  in 2025–2026; the crate pins 0.9.1, which carries every published fix (`docs/group-e2ee.md` §1).
- `cargo audit` has three documented exceptions in `crates/pigeonpost-groupcrypto/.cargo/audit.toml`:
  - RUSTSEC-2026-0330 and RUSTSEC-2026-0331 (`libcrux-kem`): lockfile-only, and CI asserts the
    crate is never compiled;
  - RUSTSEC-2026-0387 (`crypto-bigint` 0.5.5): unreachable while only suite 0x0001 is accepted.
    See `docs/security-advisory-2026-10-24.md`.
- Hosted mailboxes make the postbox an authentication service (§3 above).
- Group roles are server-enforced.
- Coverage gaps from the P-2 and Phase 3a reports:
  - groups larger than the six-device interop set have not been tested;
  - the SDS's 500/1,000-member performance targets are unmeasured;
  - browser engines are covered only through Node (WASM) and the native round-two clients;
  - Windows arm64 is built but not executed.
- Metadata (sender, timing, size padded to 32 bytes, membership) is visible to the server by design.

## 6. Build and test instructions

The crate is its own Cargo workspace on **Rust 1.91.1**. `crates/pigeonpost-groupcrypto/rust-toolchain.toml`
makes rustup install it automatically. Dependencies are pinned exactly: `openmls =0.9.1`,
`openmls_rust_crypto =0.6.0`, `openmls_basic_credential =0.6.0`, `openmls_traits =0.6.0` and
`openmls_memory_storage =0.6.0`.

```bash
git clone https://github.com/bekirdag/pigeonpost && cd pigeonpost/crates/pigeonpost-groupcrypto
cargo test --workspace --locked --release            # unit, scenario, property and vector tests
PROPTEST_CASES=3000 cargo test --locked --release --test fuzz   # CI's deeper property run
python3 -I scripts/check_vectors.py                  # vectors rechecked without Rust
cargo audit                                          # honours .cargo/audit.toml (see §5)
scripts/interop.sh                                   # Rust, WASM/Node, Kotlin/JVM, Python, C# (+ Swift on macOS)
scripts/build-wasm.sh --check                        # web bundle reproduces byte for byte (x86_64 Linux)
scripts/generate-bindings.sh --check                 # committed bindings reproduce
```

CI is `.github/workflows/group-crypto.yml`. It runs:
- fmt, clippy `-D warnings` (native and wasm32);
- the tests, the vector recheck and the 3,000-case property run;
- `cargo audit` with the lockfile-only assertion;
- the reproducible WASM bundle and bindings checks;
- interop on Linux, macOS (all six runtimes), Windows x64 and the Android AAR build.

Server-side tests are `cargo test -p pigeonpost-postbox groups` from the repository root (root
workspace, same toolchain).

## 7. Test vectors

The crate had no deterministic vectors before this packet; its scenario, interop and property
tests use fresh randomness. This packet adds known-answer vectors for every format Pigeonpost
defines itself:

- **File:** `crates/pigeonpost-groupcrypto/tests/vectors/groupcrypto-v1.txt`. It holds
  `name = value` lines in lowercase hex, except addresses, device ids, digit strings and `_ascii`
  values.
- **Generator and check:** `crates/pigeonpost-groupcrypto/tests/vectors.rs`. It recomputes every
  value from fixed seeds and compares it with the file. `PPGC_WRITE_VECTORS=1` regenerates the
  file, which should happen only after a deliberate, versioned format change.
- **Independent check:** `crates/pigeonpost-groupcrypto/scripts/check_vectors.py`, using the
  Python standard library only. It recomputes addresses, fingerprints and digits, safety numbers,
  the attachment hash and the envelope signing input from the formulas in `docs/group-e2ee.md`. CI
  runs both.

| Vector group | What it pins | Checked through |
|---|---|---|
| `address.*` | `/k/` derivation from three ed25519 identity seeds | `mailbox_for_seed`, Python reimplementation |
| `attestation.alice.*` | Attestation payload layout and the deterministic ed25519 signature | `attestation_payload` (asserted equal to the documented layout), `sign_attestation_with_seed` |
| `credential.alice.identity` | Leaf `BasicCredential` identity bytes | Built from the documented layout (the encoder is crate-private; its round trip is covered by the scenario tests) |
| `fingerprint.*`, `*.digits` | Device fingerprint and its six digit groups | `device_fingerprint`, `fingerprint_digits`, Python |
| `safety_number.*` | Two-device, three-device and empty sets; order and duplicate independence | `safety_number`, Python |
| `aad.application` | AAD bytes; parse; trailing-byte, unknown-kind and commit-kind handling | `MessageAad::decode` |
| `envelope.*` | Envelope signing input and signature over a fixed stand-in message; tamper refusal | `verify_envelope_signature`, Python (input) |
| `attachment.*` | Sealed blob with a fixed key and nonce; hash; version-byte tamper refusal | `open_attachment`, Python (hash) |

MLS objects themselves (key packages, Welcomes, commits, `PrivateMessage`s) are randomized by
design and are covered by RFC 9420 and OpenMLS's own test vectors, not by this file.

**Verified 2026-10-28** on macOS arm64 with Rust 1.91.1: `cargo test --release --locked --test
vectors` passes (2 tests), and `scripts/check_vectors.py` reports OK.

## 8. Deliverables requested from the reviewer

- Findings, each with a severity, a reproduction or argument, and an affected file and line at the
  reviewed commit.
- Answers to §4.
- An opinion on whether the threat model claims in §3 hold for the reviewed code.
- Any vectors or tests the reviewer would add.
