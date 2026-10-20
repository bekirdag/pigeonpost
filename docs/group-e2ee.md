# Group end-to-end encryption (P-2)

Status: implemented and tested as a library, bindings and protocol contract (2026-10-20).
The postbox endpoints in §7 are the contract Phase 3 implements; they do not exist yet.

This document is release prerequisite **P-2** of the big-features SDS (§3.6, §4.6): a reviewed,
cross-client group encryption implementation for persistent groups and org rooms. Groups stay
feature-disabled on any client that does not ship this library, and no client may fall back to the
custodial vault path for group content.

Code: `crates/pigeonpost-groupcrypto` (Rust, its own Cargo workspace). Web bundle:
`site-inbox/vendor/groupcrypto/`. CI: `.github/workflows/group-crypto.yml`.

---

## 1. Decision: OpenMLS 0.9.1 with the RustCrypto provider

We do not invent a protocol. Group key agreement, message protection, membership changes and
forward secrecy / post-compromise security come from **RFC 9420 Messaging Layer Security**
([RFC 9420](https://www.rfc-editor.org/rfc/rfc9420.html)). The choice is which implementation.

| | OpenMLS (Rust) | mls-rs (AWS Labs, Rust) | MLS++ / mlspp (Cisco, C++) | ts-mls (TypeScript) |
|---|---|---|---|---|
| Licence | MIT | Apache-2.0 OR MIT | BSD-2-Clause | MIT |
| RFC 9420 | Yes; MTI suite 0x0001 plus 0x0002, 0x0003 | Yes; claims full conformance | Yes (C++17, OpenSSL/BoringSSL) | "Aims to be" full |
| Independent review | No published full audit of the library. Used by XMTP, whose libxmtp (built on OpenMLS) NCC Group reviewed in Oct–Dec 2024. Active coordinated disclosure: 5 GitHub advisories 2025-09 → 2026-10, all fixed in 0.9.1 | README: "has not yet received a full security audit by a 3rd party"; no published advisories | No audit statement | README: "has not undergone a formal security audit" |
| Maintenance | Very active (0.9.1 on 2026-10-07; maintained by Phoenix R&D and partners) | Active (0.56.0 on 2026-08-19) | Active | Small project |
| WASM | `js` feature; wasm32 built in upstream CI (not tested there — we test it, §11) | Supported, Web Crypto provider "experimental" | Not targeted | Native JS |
| Mobile bindings | None upstream; UniFFI wrapper is ours | `mls-rs-uniffi` exists but last released 0.1.0 (2024-03) | Hand-written JNI/ObjC | n/a |
| Pure-Rust crypto | `openmls_rust_crypto` (RustCrypto + hpke-rs) or libcrux | RustCrypto provider marked "experimental"; stable providers are OpenSSL / AWS-LC (C) | OpenSSL (C) | WebCrypto / noble |
| MSRV | 1.91 (0.9.x) | 1.82 | — | — |

**Why OpenMLS.** It is the most actively scrutinised Rust MLS implementation that has a
production-quality *pure-Rust* provider, which is what makes one codebase reach WASM, iOS, Android,
Windows, Linux and our Rust CLI/agentd without shipping OpenSSL to every platform. Its advisory
history is a point in its favour as much as a warning: issues are found and fixed with
coordinated disclosure. mls-rs's stable providers are C libraries (OpenSSL / AWS-LC), its pure-Rust
provider is experimental, it publishes no advisory history and states that it is not
third-party audited. MLS++ would mean a C++ toolchain plus OpenSSL on every client. ts-mls would
cover only the web.

**Version pin.** `openmls =0.9.1`, `openmls_rust_crypto =0.6.0`, `openmls_basic_credential =0.6.0`,
`openmls_traits =0.6.0`, `openmls_memory_storage =0.6.0`. 0.9.1 is the only release line with fixes
for every published OpenMLS advisory: GHSA-gc79-23g3-8g52 (High, nested-extension recursion; fixed
0.8.2 / 0.9.1), GHSA-w62v-gv48-63rh and GHSA-rrmv-c79f-cf5r (Moderate, pre-auth decoding DoS; fixed
only in 0.9.0), GHSA-8x3w-qj7j-gqhf (High, tag length validation; fixed 0.7.2) and
GHSA-qr9h-x63w-vqfm (Moderate, secret-tree persistence; fixed 0.7.1). 0.9 requires Rust 1.91, so
the crate is a separate Cargo workspace with its own `rust-toolchain.toml`. Since Phase 3a the root
workspace is on 1.91.1 too, because the postbox (server-side validation) and the CLI link this
crate directly.

**Ciphersuite.** `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (0x0001), the RFC 9420
mandatory-to-implement suite. It is the only suite this crate creates or accepts.

**Licences.** Everything linked is MIT, Apache-2.0, BSD-2/3, Unicode-3.0 or MPL-2.0 (UniFFI
runtime and `hpke-rs`). MPL-2.0 is file-level copyleft: using the unmodified crates in an MIT
project and in App Store / Play / Microsoft Store builds is fine; modifications to MPL files would
have to be published. No GPL code is linked.

**Size.** Release builds (`opt-level = "z"`, LTO, stripped): macOS dylib 2.0 MB; Android
`.so` 2.5 MB (arm64-v8a), 1.7 MB (armeabi-v7a), 3.1 MB (x86_64), AAR 3.6 MB; web bundle 1.73 MB
wasm (0.54 MB gzip) plus 54 KB JS glue.

### Reservations, stated plainly

* **No independent audit of OpenMLS itself is public.** The NCC Group engagement reviewed libxmtp
  on top of OpenMLS, not OpenMLS as a library. Two High-severity advisories in 2026 show that
  real bugs remain to be found. Our protocol profile (§2) disables the features those advisories
  needed (public messages, proposals by reference, external commits), but that is mitigation,
  not assurance.
* **Our glue is new code.** The credential binding, AAD, payload framing, state format and the
  binding layers in this crate have been reviewed only by their author (§13). An external review
  of `crates/pigeonpost-groupcrypto` and this document is recommended before groups reach GA.
* **WASM is tested by us, not upstream.** OpenMLS builds wasm32 in CI but does not run tests there.
  Our Node interop run exercises the shipped bundle end to end; browser engines are not yet in CI.

---

## 2. Protocol profile

| Setting | Value | Why |
|---|---|---|
| Ciphersuite | 0x0001 only | MTI; X25519/Ed25519 match Pigeonpost's existing key types. |
| Wire format | `PURE_CIPHERTEXT` for application *and* handshake messages | Commits are `PrivateMessage`s: the server sees group id, epoch, content type and our AAD, not who was added or removed at the MLS layer. Also excludes the public-message preconditions of GHSA-8x3w. |
| Proposals | Inline in commits only; standalone proposals are rejected | No proposal-by-reference state to race on. |
| External commits / joins | Not used | Every join is a Welcome from an existing member device (§7.4). |
| PSKs, GroupContextExtensions proposals | Rejected | Not needed. |
| Ratchet tree | Carried in the Welcome (`ratchet_tree` extension) | No separate tree download endpoint. |
| Past epochs kept | 0 (`max_past_epochs = 0`) | The server only accepts messages for the current epoch and sequences them before the next commit, so old-epoch secrets are never needed; deleting them maximises forward secrecy. |
| Padding | 32-byte blocks | Hides exact lengths within a block. |
| Commits | Always include an update path (`force_self_update`) | Every commit refreshes the committer's leaf: post-compromise security on each membership change. |
| Key packages | Single-use, plus one last-resort package per device | RFC 9420 §16.8 / last_resort extension. |
| Credential | `BasicCredential` with Pigeonpost identity bytes (§3) | Self-certifying binding to the `/k/` mailbox. |

`enc_scheme` on the wire (SDS §4.3 `group_messages.enc_scheme`): `mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1`.

---

## 3. Identity: device credentials bound to the mailbox

A Pigeonpost group member is a **mailbox** (`/k/<26 base32>`, SDS §4.1). Each mailbox may have
several **devices** (phone, laptop, web, agentd host); every device is its own MLS leaf with its
own Ed25519 signature key that never leaves the device.

**Credential identity bytes** (`BasicCredential.identity`):

```
u8   version = 1
u16  len || mailbox address      canonical "/k/…"
u16  len || device id            1–64 of [A-Za-z0-9._-], unique within the mailbox
[32] mailbox Ed25519 identity public key
[64] Ed25519 attestation signature by that key
```

**Attestation payload** signed by the mailbox identity key:

```
"pigeonpost/groupcrypto/device-attestation/v1\0"
u16 len || mailbox || u16 len || device id || u16 ciphersuite (0x0001) || u16 len || device signature key
```

**Verification** (every leaf, on key-package inspection, Welcome, every commit, every message
sender):

1. the address is canonical and equals `Address::from_pubkey(mailbox key)` — `/k/` addresses are
   the hash of the key, so no directory lookup is involved (same derivation as `pigeonpost-core::address`, pinned by `tests/address.rs`);
2. the attestation verifies (`verify_strict`) for exactly the leaf's MLS `signature_key`;
3. within a group, `(mailbox, device id)` is unique;
4. a leaf update (update proposal or commit path) must keep the same mailbox, device id **and**
   signature key. Device keys never rotate in place; a new key is a new device.

A commit failing any check is not merged: that device stops at the epoch and reports a fork
(it fails closed; it can be re-added).

**Who signs the attestation.**

* Self-custodied mailboxes (CLI, agentd, local identity files): the client signs locally
  (`Device::new_with_identity_seed`, or `attestation_payload()` + external signer +
  `install_attestation()`).
* Hosted mailboxes: the mailbox identity key is held by the postbox (SDS §3.6 calls this path
  custodial). The postbox signs the payload for an authenticated device enrolment (§7.1). **This
  makes the postbox an authentication service that could mint a device for a hosted mailbox.**
  The countermeasures are device-list transparency and verification (§9), not cryptography alone.

**Fingerprints and safety numbers.**
`device_fingerprint = SHA-256("pigeonpost/groupcrypto/device-fingerprint/v1\0" || len-prefixed mailbox, device id, signature key)`,
shown as six groups of five digits. A group **safety number** hashes the sorted set of current
device fingerprints (SHA-512, twelve groups of five digits); it is identical on every member iff
they see the same device list. The MLS **epoch authenticator** additionally lets two members
confirm they share the same epoch (fork detection).

---

## 4. Library API (`pigeonpost-groupcrypto`)

All bindings expose the same surface (Rust `Device`; UniFFI/WASM `GroupDevice`).

| Area | Operations |
|---|---|
| Device | `Device::new(mailbox, device_id)`, `attestation_payload()`, `install_attestation(mailbox_pk, sig)`, `new_with_identity_seed(seed, device_id)`, `mailbox()`, `device_id()`, `signature_public_key()`, `fingerprint()`, `is_attested()`, `groups()` |
| Key packages | `generate_key_packages(n ≤ 100)`, `generate_last_resort_key_package()`; server-side `inspect_key_package(bytes) → {key_package_ref, mailbox, device_id, signature_key, fingerprint, last_resort, not_after}` |
| Groups | `create_group(group_id)` (epoch 0, creator only), `join_from_welcome(welcome)`, `members()`, `epoch()`, `safety_number()`, `epoch_authenticator()`, `forget_group()` |
| Commits | `commit(group, key_packages, remove)`, `add_members`, `remove_members`, `remove_mailbox`, `self_update` → `CommitBundle {epoch, commit_id, commit, welcome?, welcome_recipients, envelope_signature, added, removed}`; then `confirm_commit()` (server accepted) or `discard_commit()` (server rejected). One pending commit at a time. |
| Messages | `encrypt_message(group, client_message_id, body, attachments) → OutgoingMessage {epoch, ciphertext, envelope_signature}`; `process_incoming(group, bytes) → Message | Commit | OwnCommitConfirmed | HistoryOffer | Own` |
| Attachments | `seal_attachment(bytes) → {sealed, reference{key, sealed_sha256, plaintext_len}}`, `open_attachment(sealed, reference)` |
| History | `seal_history(self_group, id, archive)`, `open_history(offer, sealed_archive)` (§10) |
| Persistence | `export_state(key32)`, `Device::import_state(blob, key32)`, `generate_state_key()` |
| Server helpers | `inspect_envelope(bytes)`, `verify_envelope_signature(device_key, bytes, sig)`, `welcome_recipients(welcome)` |

Errors carry a stable `code`: `invalid_input`, `decode`, `not_attested`, `invalid_credential`,
`unknown_group`, `group_exists`, `pending_commit`, `no_pending_commit`, `stale_epoch`,
`future_epoch`, `not_member`, `duplicate_device`, `member_not_found`, `binding_mismatch`,
`decryption_failed`, `not_self_group`, `unsupported`, `mls`.

Misuse resistance built in: unattested devices cannot publish or join; key packages are validated
(signature, lifetime, suite, attestation, duplicates) before any commit is built; a second commit
cannot be started while one is pending; a commit cannot remove its own author; own echoed
messages are recognised; a pending commit is auto-cleared when another commit for the same epoch
is processed; state import re-verifies the device credential.

---

## 5. Message framing

### 5.1 Authenticated data (AAD)

Every handshake and application message created by this crate sets the MLS `authenticated_data`
to:

```
"PPGC" || u8 version=1 || u8 kind (1 application, 2 commit)
|| u16 len || group id || u16 len || sender mailbox || u16 len || sender device
|| u64 epoch || u16 len || client message id (commit id for commits)
```

The AAD travels in clear inside the `PrivateMessage` and is covered by the AEAD and the sender's
signature. Recipients reject a message unless the AAD's group, epoch and kind match the MLS header
and the AAD's mailbox/device match the **authenticated** sender leaf credential. The client message
id is known before the server assigns a sequence number (SDS §4.6) and is repeated inside the
encrypted payload; both copies must agree.

### 5.2 Encrypted payload

```
u8 version=1 || u8 kind (1 message, 2 history offer) || u16 len || client message id
|| u32 len || body (≤ 1 MiB; Pigeonpost's JSON message document: text, filenames, reply/edit metadata)
|| u16 n (≤ 32) || n × ( [32] attachment key || [32] SHA-256 of sealed blob || u64 plaintext length )
```

### 5.3 Envelope signature

`envelope_signature = Ed25519_sign(device signature key, "pigeonpost/groupcrypto/envelope/v1\0" || SHA-256(serialized MLS message))`.
The postbox verifies it against the device key registered for `(mailbox, device_id)` (§7.1), so it
can authenticate the posting device and bind the cleartext header without decrypting. The domain
string cannot collide with MLS's `SignWithLabel` encoding (which begins with a length-prefixed
`"MLS 1.0 "` label).

### 5.4 Replay and duplicates

MLS deletes each message key after first use, so a replayed ciphertext fails to decrypt
(`decryption_failed`). Clients still deduplicate on `(sender mailbox, client_message_id)` and on
server `seq`, and keep decrypted plaintext in their own encrypted local store: **a device cannot
re-decrypt a message it already processed**, and the sender never decrypts its own messages.

---

## 6. Objects the server stores and sees

| Object | Stored as | Server sees | Server cannot |
|---|---|---|---|
| Device record | `group_devices(mailbox, device_id, signature_key, fingerprint, created_at, revoked_at)` | Which devices a mailbox has | Use the device key (no private key) |
| Key package | `group_key_packages(ref, mailbox, device_id, bytes, last_resort, not_after, claimed_at)` | Device identity, HPKE init public key, lifetime | Decrypt anything sent to it |
| Commit | `group_handshakes(group_id, seq, epoch, commit_id, sender, sender_device, bytes)` | Group, epoch, sender, size, timing; the declared membership change (§7.3) | Learn epoch secrets; forge or alter a commit undetected |
| Welcome | `group_welcomes(id, group_id, epoch, key_package_ref, recipient device, bytes)` | That a device was added, when | Read group secrets (HPKE-encrypted to the device's init key) |
| Application message | `group_messages` (SDS §4.3) with `epoch`, `enc_scheme`, `client_message_id`, `sender`, `sender_device` | Sender, epoch, size (32-byte padded), time, attachment blob ids | Read body, filenames, attachment keys |
| Epoch acknowledgement | `group_epoch_acks(group_id, mailbox, device_id, epoch, at)` | How far each device has processed | — |
| Sealed attachment / history archive | blob store, addressed by SHA-256 of the sealed bytes | Size, uploader, referencing message | Decrypt (key is inside the MLS message) |

The server never holds group private keys, epoch secrets, attachment keys or device signature
private keys. It does hold hosted mailboxes' identity keys (§3, §12).

---

## 7. Postbox API that Phase 3 must add

All routes are `/v1`, require the acting mailbox (`x-pigeonpost-identity`) and, for device-scoped
routes, `x-pigeonpost-device: <device_id>` of a registered, unrevoked device of that mailbox.
Bodies are JSON; byte fields are standard base64. Every mutating route takes a `request_key`;
the server keeps `(principal, group_id, action, request_key, body_hash)` idempotency records (SDS
§4.3): a replay with the same body returns the original response, the same key with a different
body returns `409 idempotency_conflict`. All new routes are rate-limited and audit-logged.

### 7.1 Devices and key packages

| Method | Path | Body / query | Result and rules |
|---|---|---|---|
| POST | `/v1/mls/devices` | `{device_id, signature_key, attestation?}` | Registers a device. Self-custodied mailboxes include the attestation they signed; for hosted mailboxes the server verifies the session (step-up per SDS §3.4 for the second and later device), signs the attestation payload with the custodial identity key and returns `{attestation, mailbox_public_key}`. `409 device_exists` if the id is taken. Emits `group.device {mailbox, device_id, fingerprint, change:"added"}` to the mailbox's own account stream **and** to every co-member of its groups. |
| GET | `/v1/mls/devices?mailbox=` | | Active devices `[{device_id, fingerprint, signature_key, created_at}]` of a mailbox the caller shares a group or contact relationship with. Used for safety-number and device-change UI. |
| DELETE | `/v1/mls/devices/{device_id}` | `{request_key}` | Revokes one of the caller's devices (step-up). Deletes its unclaimed key packages, rejects further posts from it, and marks a **pending removal** of that leaf in every group it is in (§7.5). |
| POST | `/v1/mls/key-packages` | `{request_key, key_packages:[b64], last_resort?:b64}` | Upload from the acting device. Each package must pass `inspect_key_package`, name the acting mailbox and device, carry the registered signature key and an acceptable lifetime. At most 100 unclaimed per device; one last-resort package per device (replaces the previous one). |
| GET | `/v1/mls/key-packages/count` | | `{unclaimed, last_resort:bool}` for the acting device; clients top up below 10. |
| POST | `/v1/groups/{id}/key-packages/claim` | `{request_key, mailboxes:[address]}` | For each active device of each named mailbox, atomically claims one single-use package (or the last-resort package when none remain) and returns `[{mailbox, device_id, key_package}]`. Authorized only for a device of an active member who may perform the pending membership operation (admin/inviter for invitations; any active member device for `keying` acceptances and own-device additions; see §7.4). Claims expire after 10 minutes if no commit consumes them. |

### 7.2 Group handshake delivery

| Method | Path | Body / query | Result and rules |
|---|---|---|---|
| POST | `/v1/groups/{id}/commits` | `{request_key, epoch, commit_id, commit, envelope_signature, welcome?, adds:[{mailbox, device_id, key_package_ref}], removes:[{mailbox, device_id}], expected_revision}` | See §7.3. `201 {seq, epoch, revision}`. |
| GET | `/v1/groups/{id}/messages` | `?after_seq=&limit≤100` | SDS §4.5 route, extended: items are `{seq, type: "application"|"commit"|"notice", epoch, sender, sender_device, ciphertext|commit|notice}` in one per-group sequence. A device is served commits from its join interval onward and application messages within its visible intervals. Items posted by the requesting device are included (clients recognise them, §5.4). |
| GET | `/v1/mls/welcomes` | | Pending Welcomes for the acting device: `[{welcome_id, group_id, epoch, seq, welcome}]`. `seq` is the commit's sequence: the device starts reading the group after it. |
| POST | `/v1/mls/welcomes/{welcome_id}/ack` | `{request_key, joined: bool, error_code?}` | Device consumed (or could not use) the Welcome. A `joined:false` report queues a re-add with a fresh key package. |
| POST | `/v1/groups/{id}/epoch-ack` | `{epoch}` | Highest epoch the acting device has fully processed. Monotonic; drives §7.6. |
| GET | `/v1/groups/{id}/crypto` | | `{epoch, revision, my_devices, pending: [{kind: "add"|"remove"|"self_update", mailbox, device_id, since}], lagging_devices:[{mailbox, device_id, acked_epoch}]}` — what an online authorized device should commit next. |

`POST /v1/groups/{id}/messages` (SDS §4.5) additionally requires `sender_device`, `enc_scheme`
(= §2 string) and `envelope_signature` instead of a separate signature field, and is validated as
in §7.3 steps 1–3 with `kind = application`. It is refused with `409 epoch_conflict
{current_epoch}` if `epoch` ≠ the group's epoch, and with `409 key_activation_pending` while the
group has a pending *removal* (SDS §4.6: "requires a committed key update before further encrypted
application sends").

### 7.3 Commit acceptance (one transaction)

1. The acting device is registered, unrevoked, and an active (or, for its own Welcome-less
   activation, `keying` creator) member device of the group.
2. `verify_envelope_signature(device key, commit, envelope_signature)`.
3. `inspect_envelope(commit)`: kind `commit`, group id = `{id}`, sender mailbox/device = the acting
   ones, `client_message_id` = `commit_id`.
4. **Serialization:** `epoch` (and the header epoch) must equal `groups.epoch`, and
   `expected_revision` must equal `groups.revision`. Otherwise `409 revision_conflict
   {current_epoch, revision}`. The first valid commit for epoch N wins; every other commit for N
   is rejected. There is no merge.
5. **Authorization of the declared change:** `adds` are claimed key packages (§7.1) for members
   in `keying`/`invited→accepted` state or for new devices of active members; `removes` are
   members/devices that are `removed`, `banned`, `left`, revoked or pending removal — or the
   committer may remove its own mailbox's other devices. Removing an active member who is not
   pending removal requires the membership PATCH (admin) in the *same* request flow
   (`expected_revision` makes that atomic). `welcome_recipients(welcome)` must equal exactly the
   `key_package_ref`s of `adds`; a commit with adds must carry a Welcome. Agents never commit
   role-bearing changes (`human_required`, SDS §4.4); they may commit their own self-updates and
   pending removals.
6. Apply atomically: allocate `seq` from the group sequence, store the commit, store one Welcome
   row per added device, set `groups.epoch = epoch + 1`, bump `revision`, move members
   `keying → active`, close membership intervals for removed members, clear satisfied pending
   operations, set `activation_state = active` for the initial commit, append notices, emit events.

The server cannot verify inside the encrypted commit that it really contains exactly the declared
adds and removes. Honest clients check this independently: each `Commit` outcome lists the added
and removed devices, and clients compare them with the server's membership metadata and notices;
a mismatch is shown as a security warning and reported (§9).

### 7.4 Membership flows

* **Create / activate.** `POST /v1/groups` creates the row (`activation_state = provisioning`,
  `epoch = 0`). The creator device calls `create_group(id)`, claims packages for its own other
  devices and for accepted invitees, and posts the first commit (epoch 0 → 1). Org rooms created
  while the owner is offline stay `provisioning` until then (SDS §4.3).
* **Invitation accepted / public join.** The member moves to `keying` (no history, no calls). Any
  online authorized device sees a pending `add` in `GET /crypto`, claims the new member's packages
  for **all** their active devices and commits the add. On acceptance of the Welcome the member
  becomes `active`.
* **New device of an existing member.** After `POST /v1/mls/devices`, a pending `add` for that
  device appears in each of the mailbox's groups. Preferably one of the same mailbox's other
  devices commits it (it can display the fingerprint for comparison first); otherwise any member
  device may. The new device gets no history (§10).
* **Leave.** The leaving client deletes its local state; the server marks the member `left` and
  queues a pending removal; any online member device commits it.
* **Remove / ban / offboarding / account deletion.** Server access stops immediately (SDS §4.6);
  a pending removal blocks application sends (`key_activation_pending`) until an authorized device
  commits it. Removed devices that process the commit learn only that they were removed.
* **Lost device / revocation.** `DELETE /v1/mls/devices/{id}` from another device of the mailbox;
  pending removals as above. A revoked device's later posts are refused even before the removal
  commit lands.
* **Device that lost its state.** It registers as a new device (new id or the same id after
  revocation of the old key) and is re-added; it gets no history.

### 7.5 Concurrent commits

Commits race whenever two devices act on the same pending operation, or a self-update meets a
membership change. Resolution is entirely server-ordered:

1. Device A and device B both build commits on epoch N.
2. A's commit is accepted (epoch N+1). B's receives `409 revision_conflict`.
3. B calls `discard_commit`, fetches and processes A's commit (`process_incoming`; a still-pending
   commit for N is also auto-cleared there), re-reads `GET /crypto`, and rebuilds only what is
   still needed, with a new `request_key`/`commit_id`. Retry with jittered exponential backoff
   (base 200 ms, cap 10 s).
4. Claimed but unused key packages expire after 10 minutes and are not re-issued (single-use);
   a fresh claim is made for the rebuilt commit.

A device that posted a commit and crashed before seeing the verdict recognises its own commit when
it next reads the group: `process_incoming` returns `OwnCommitConfirmed` and merges the pending
state; if a different commit won, the pending one is discarded.

### 7.6 Ordering, offline replay and retention

* Commits, application messages and notices share one per-group `seq`. Clients process strictly in
  `seq` order. A message from a later epoch means a commit was skipped (`future_epoch`): fetch the
  gap. A message from an earlier epoch (`stale_epoch`) cannot occur through the server; drop it.
* Offline devices catch up by replaying everything after their cursor; tolerance within an epoch
  follows OpenMLS defaults (out-of-order tolerance 5, maximum forward distance 1000).
* The server keeps handshake rows until every member device has acknowledged a later epoch
  (`epoch-ack`), or for 90 days, whichever is first. A device that comes back after its missing
  commits were pruned is **desynced**: it gets `410 resync_required`, deletes the group state and
  is re-added as in §7.4 (no history).
* Devices that have not acknowledged for 30 days are reported as `lagging_devices` and shown to the
  group; owners/admins may remove them. Disappearing-message expiry (SDS §17) deletes application
  rows but never commits a device still needs.

### 7.7 Events (SDS §21.3 additions)

`group.handshake {group_id, seq, epoch}`, `group.crypto_pending {group_id, kind}`,
`group.device {mailbox, device_id, fingerprint, change: "added"|"revoked"}`,
`mls.welcome {welcome_id, group_id}` (device-scoped), `mls.key_packages_low {device_id}`.
Payloads never contain ciphertext or keys. Errors added to SDS Appendix A:
`epoch_conflict`, `key_activation_pending`, `resync_required`, `device_exists`,
`device_revoked`, `idempotency_conflict`, `key_packages_exhausted`.

---

## 8. Attachments

Each attachment is sealed by the sender with a fresh 256-bit key (`seal_attachment`):
`"PPGA" || u8 1 || nonce[24] || XChaCha20-Poly1305(key, nonce, plaintext, aad = header)`. The key,
the SHA-256 of the sealed blob and the plaintext length travel inside the MLS message; filenames
and MIME types travel in the encrypted body. Recipients check the hash before decrypting. Upload
and claim rules are SDS §4.6: upload binds the uploader and quota, the message post binds blob ids
to `(group_id, seq)` transactionally, and reads require a live reference visible to the reader.

---

## 9. Making membership changes visible

* Every `Commit` outcome names the committer device and the added/removed devices from the
  **verified** commit. Clients render system notices from these, not from server notices; the
  server's notice is only used to cross-check, and a disagreement is shown as a warning.
* When a mailbox in a group gains a device, other members see "Jane added a new device" with the
  fingerprint available. A hosted mailbox can be given a device by a compromised server (§12), so
  the UI must make this visible, not silent.
* Group info shows the safety number (§3) and each member's device fingerprints; two people
  comparing the safety number in person or over a call verify the whole device list at once.
  Clients remember verified fingerprints per contact and flag changes.
* Clients may compare `epoch_authenticator` values over an authenticated side channel (a
  Pigeonpost direct message or a call) to detect a server that forks the group.

---

## 10. History policy and recovery

* **No history by default** (SDS D-08). A device joins at the current epoch and can decrypt only
  messages sent after that. Rejoining starts a new interval.
* **Own-device history transfer** is an explicit, user-initiated operation between devices of the
  *same mailbox*: the old device's user chooses "transfer history to my new device" after comparing
  the new device's fingerprint. The devices use a **self group** (an MLS group whose members are
  only that mailbox's devices; the library enforces this on both send and receive,
  `not_self_group`). The old device packs its locally stored plaintext history into an archive,
  `seal_history` seals it under a fresh key, uploads the sealed blob and sends the key in the self
  group; the new device receives a `HistoryOffer` and calls `open_history`. The server stores only
  the sealed archive. Transfers to other mailboxes are impossible through this path.
* **Lost all devices.** No recovery of group secrets is possible by design; the mailbox re-enrols a
  device and is re-added to its groups without history.

---

## 11. Bindings, artefacts and how they are tested

| Platform | Binding | Artefact / build | Tested |
|---|---|---|---|
| Rust (CLI, agentd, postbox validation) | crate API | `cargo build` in `crates/pigeonpost-groupcrypto` | Unit/integration tests, property tests, interop agent (Linux, macOS, Windows x64) |
| Web inbox | wasm-bindgen 0.2.128, `--target web` | `site-inbox/vendor/groupcrypto/` (committed, reproducible, SHA-256 pinned; `scripts/build-wasm.sh`) | Interop agent runs the committed bundle through its loader in Node 24 (Linux, macOS, Windows) |
| iOS / iPadOS / macOS | UniFFI 0.31.0 Swift | `PigeonpostGroupCryptoFFI.xcframework` (iOS arm64, simulator arm64+x86_64, macOS arm64+x86_64) + SwiftPM package `apple/` (`scripts/build-apple.sh`) | Interop agent on macOS; `swift test` (XCTest) against the XCFramework |
| Android | UniFFI Kotlin (JNA) | AAR with `arm64-v8a`, `armeabi-v7a`, `x86_64`, API 26, 16 KB-aligned (`scripts/build-android.sh`) | Built in CI; the Kotlin binding itself is exercised on a desktop JVM in the interop run (Linux, macOS). **Not yet executed on an Android device/emulator.** |
| Linux desktop | UniFFI Python | `bindings/python/pigeonpost_groupcrypto.py` + `libpigeonpost_groupcrypto_ffi.so` | Interop agent (Linux, macOS, Windows) |
| Windows | uniffi-bindgen-cs v0.11.1 (C#, P/Invoke) | `runtimes/win-x64|win-arm64/native/pigeonpost_groupcrypto_ffi.dll` + `pigeonpost_groupcrypto.cs` (`scripts/build-windows.ps1`) | Interop agent with .NET 8 on Windows x64, Linux and macOS; arm64 is built, not executed |

Generated binding sources are committed under `crates/pigeonpost-groupcrypto/bindings/` and CI
regenerates and diffs them (`scripts/generate-bindings.sh --check`).

**Interop scenario** (`interop/run_interop.py`, driven by `scripts/interop.sh`): each runtime is a
separate process speaking a small JSON-lines protocol; the script plays the postbox with the §7
rules, and its server-side checks run in the native Rust agent (the code the postbox will link).
Steps: enrol six devices (Rust, WASM/Node, Swift, Kotlin/JVM, Python, C#) in five mailboxes and
validate their key packages → one device creates the group and adds the others in one commit
(Welcome routed by KeyPackageRef) → every device sends, every other decrypts (one with an
attachment) → a member is removed; it learns only of its removal, cannot decrypt later traffic,
and even its pre-removal state snapshot fails with `future_epoch` → a new device of an existing
mailbox is added and cannot read earlier messages (`stale_epoch`) → two devices commit
concurrently; the second gets `revision_conflict`, discards, processes the winner and retries; a
message encrypted before both commits is refused (`epoch_conflict`) and re-encrypted under the same
client id → every device rotates its keys in turn, round-trips its encrypted state, and sends → an
offline device replays the whole backlog in order → all devices agree on epoch, epoch
authenticator and safety number. The script runs twice with the agent order reversed so each
binding takes each role.

**Property tests** (`tests/fuzz.rs`, 300 cases locally, 3000 in CI): random bytes into every
parser and into `process_incoming`/`join_from_welcome`/`import_state`; bit-flipped and truncated
application messages, commits, Welcomes and key packages. Invariants: no panic; a mutated message
never decrypts to anything but the original; the original still decrypts afterwards; any key
package that still verifies is still the same attested device.

Local evidence (2026-10-20, macOS arm64): 10 Rust tests pass; the interop scenario passes with all
six runtimes in both orders (epoch 10, 6 devices, 5 mailboxes); `swift test` passes against the
XCFramework; the Android AAR builds with NDK 25.1; the committed WASM bundle is the x86_64 Linux build, which CI reproduces byte for byte.

---

## 12. Threat model

**Assets:** message bodies, attachments and filenames; group secrets; device signature keys;
local state blobs; membership integrity; metadata.

| Adversary | Can | Cannot (given the profile above) |
|---|---|---|
| Network attacker | See TLS-protected traffic sizes and timing | Read or modify anything (TLS, then MLS) |
| **Malicious or compromised postbox / delivery service** | See all metadata in §6 (who is in which group, which devices, when and how much is sent, sender of each message, attachment sizes); delay, drop, reorder within its own sequence, or refuse delivery; deliver different commit streams to different members (forking the group — detectable by epoch authenticator / safety number comparison, §9); deny service; keep ciphertext beyond retention | Decrypt application messages, attachments or history archives; forge a message or commit from a member device; add a device to a **self-custodied** mailbox; make a removed device able to decrypt post-removal traffic; make a member accept a credential not attested by its mailbox key |
| **Authentication-service compromise** (the postbox holding *hosted* mailbox identity keys, or the registry mapping handles to `/k/` addresses) | Mint an attested device for a hosted mailbox and have it added to that mailbox's groups (it then reads new messages); point a handle at a different `/k/` address before a group invite | Do so invisibly: the new device appears in device lists, notices and the safety number (§9). Users who verified fingerprints/safety numbers detect it. Self-custodied mailboxes (CLI, agentd) are not affected by custodial key compromise. Past messages stay protected (no history to new devices). |
| Malicious group member | Read everything sent while they are a member; leak it; post as themselves; attempt invalid commits | Impersonate another member; read messages after removal; add an unattested device (honest clients reject the commit and fork off, failing closed) |
| Removed member / revoked device | Keep what it already decrypted | Decrypt anything from later epochs, even with a saved state copy (tested) |
| Stolen device (unlocked) | Everything that device can do until revoked | Read messages after revocation and the removal commit |
| Stolen state blob | Nothing without the state key; with the key, the secrets of the epoch it was taken at (forward secrecy covers older epochs, post-compromise security recovers after the next commit by that device) | — |

**Not protected, by design or for now:** metadata listed above; group roles and posting policy are
enforced by the server, not cryptographically (a malicious server could let a non-admin's commit
through; honest clients show who made each change); message deletion/edit "for everyone" and
disappearing timers are best-effort on other members' devices (SDS §17); hosted-mailbox device
minting is detectable, not preventable; denial of service; traffic analysis beyond 32-byte padding.

---

## 13. Security self-review

Reviewed by the implementer on 2026-10-20. Findings and residual risks:

1. **Library assurance (moderate concern).** OpenMLS has no published independent library audit
   and had two High advisories in 2026. Mitigations: exact pins on the fully patched 0.9.1, a
   profile that disables public messages, proposals by reference, external commits and PSKs, CI
   property tests over our message-processing surface. Action: subscribe to OpenMLS advisories;
   commission an external review of this crate + protocol before GA.
2. **Hosted mailboxes make the postbox an authentication service.** Stated in §3/§12; the defence
   is visibility (device notices, fingerprints, safety numbers), which depends on Phase 3 UI work.
   Owners of self-custodied mailboxes are not exposed.
3. **Server-enforced roles.** MLS here does not encode group roles; a compromised server can
   accept unauthorized membership commits from a *member*. Clients display the committer of every
   change.
4. **Envelope signature reuses the device's MLS signature key** for a second, domain-separated
   purpose (§5.3). Acceptable because the prefix cannot parse as an MLS `SignContent`, but it is a
   deliberate key reuse to note for reviewers.
5. **State at rest is one blob.** Callers must store the 32-byte state key in the platform keystore
   and overwrite the blob after every mutating call; an old blob retains old epoch secrets.
   There is no partial-write protection inside the library; clients write atomically
   (temp file + rename / single IndexedDB transaction).
6. **Last-resort key packages** may be used for more than one Welcome, weakening forward secrecy of
   those initial joins until the device's first self-update. Clients self-update after joining via a
   last-resort package and top up single-use packages (`mls.key_packages_low`).
7. **Post-compromise security needs updates.** Commits always carry an update path, but an idle
   member's leaf only refreshes when it commits. Clients should `self_update` at least every 7 days
   of activity or 1,000 sent messages.
8. **Lockfile-only advisories.** `cargo audit` reports RUSTSEC-2026-0330/0331 in `libcrux-kem
   0.0.9`, listed in `Cargo.lock` as an optional dependency of `hpke-rs-libcrux` which no build
   enables. Both are ignored in `crates/pigeonpost-groupcrypto/.cargo/audit.toml` and CI fails if
   either crate ever becomes part of a build graph. `proc-macro-error2` (unmaintained, build-time
   macro dependency) is a warning only.
9. **Supply chain of the web bundle.** Built reproducibly (byte-identical across rebuilds and
   checkout paths on the x86_64 Linux reference host, checked in CI; a macOS build differs only in
   layout because Cargo hashes the host triple into crate metadata), SHA-256 pinned in `integrity.js` and checked by the loader before
   instantiation; served same-origin under `script-src 'self' 'wasm-unsafe-eval'`. The glue uses
   no `eval`/`new Function` (wasm-bindgen 0.2.128).
10. **Coverage gaps.** Not yet run: Android device/emulator, iOS simulator/device, Windows arm64
    execution, browser engines (only Node), groups larger than six devices, performance at the SDS
    500/1,000-member targets. These are Phase 3 test items.
11. **Decode hardening.** Our codecs are length-prefixed, bounds-checked, reject trailing bytes and
    never allocate beyond the input; body ≤ 1 MiB, ≤ 32 attachments, group id ≤ 64 bytes.
    Property tests found no panics.

---

## 14. Building and testing

```bash
cd crates/pigeonpost-groupcrypto          # rustup picks up Rust 1.91.1 from rust-toolchain.toml
cargo test --workspace --locked --release  # unit, scenario and property tests
scripts/build-wasm.sh [--check]            # needs wasm-bindgen-cli 0.2.128
scripts/generate-bindings.sh [--check]     # needs uniffi-bindgen-cs v0.11.1+v0.31.0
eval "$(scripts/fetch-kotlin-tools.sh /tmp/kt)"   # pinned kotlinc 2.2.21 + JNA 5.17.0, hash-checked
scripts/interop.sh                         # all agents this host can run
scripts/build-apple.sh && (cd apple && swift test)
ANDROID_NDK_HOME=… scripts/build-android.sh
pwsh scripts/build-windows.ps1
```

## 15. Sources

* RFC 9420, The Messaging Layer Security (MLS) Protocol — https://www.rfc-editor.org/rfc/rfc9420.html
* OpenMLS repository and README (licence, ciphersuites, providers, WASM status) — https://github.com/openmls/openmls
* OpenMLS security advisories — https://github.com/openmls/openmls/security/advisories
  (GHSA-gc79-23g3-8g52, GHSA-w62v-gv48-63rh, GHSA-rrmv-c79f-cf5r, GHSA-8x3w-qj7j-gqhf, GHSA-qr9h-x63w-vqfm)
* crates.io release data for `openmls`, `openmls_rust_crypto`, `mls-rs`, `mls-rs-uniffi`, `uniffi`, `wasm-bindgen` (queried 2026-10-20)
* mls-rs repository and security notice — https://github.com/awslabs/mls-rs ; advisories page (none published) — https://github.com/awslabs/mls-rs/security/advisories
* NCC Group, Public Report – XMTP MLS Implementation Review (2024) — https://www.nccgroup.com/research/public-report-xmtp-mls-implementation-review/
* Cisco MLS++ — https://github.com/cisco/mlspp ; ts-mls — https://github.com/LukaJCB/ts-mls
* uniffi-bindgen-cs (versions and configuration) — https://github.com/NordSecurity/uniffi-bindgen-cs
* wasm-bindgen changelog (deterministic output from 0.2.116) — https://github.com/wasm-bindgen/wasm-bindgen/blob/main/CHANGELOG.md
* Pigeonpost big-features SDS §3.6, §4, §20, §26 (internal planning document)
