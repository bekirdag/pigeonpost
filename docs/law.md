# Pigeonpost — Lawful access

Status: engineering control specification. The repository implements the cryptographic and runtime
controls described here; production activation still requires the counsel, custody, witness, and
operator prerequisites in §8. Governs `core` (envelope), `loft` (trace log), `registry` (compliance
key list), and the operator runbook.
Opened: 2026-08-08

Pigeonpost operates in the United States, the European Union, and Türkiye. This document states what
those three regimes actually require, what we chose to build beyond that, and — because it is the
part that will be misread — what we deliberately did **not** build.

Read `architecture.md` and `spam.md` first. This document changes one of their load-bearing claims,
and says so plainly in §3.

> [!IMPORTANT]
> Legal analysis here is engineering input prepared from primary sources, not legal advice. Nothing
> ships until counsel in each jurisdiction has reviewed §1. The open questions are in §8.

## 1. What the law requires

This is a working matrix for engineering. Provider classification, territorial reach, and the
applicable member-state rules remain counsel decisions; a row is not an instruction to disclose.

| | **United States** | **European Union** | **Türkiye** |
| --- | --- | --- | --- |
| General standing retention | No general federal mandate identified | No general Union-wide mandate; national rules can differ | Conditional on classification as a *yer sağlayıcı* — 5651 Art. 5 |
| Retain what | No standing dataset defined here | No standing dataset defined here | In-scope service traffic data if the classification applies |
| For how long | — | — | Statutory band **1–2 years**; the exact current rule is a §8 counsel gate |
| Freeze on request | §2703(f) — 90 days, +90 on renewal | EPOC-PR — 60 days | On order |
| Produce on order | Subpoena / §2703(d) / warrant by data type | EPOC — **10 days**, **8 hours** emergency | Prosecutor or court |
| Voluntary disclosure | Generally prohibited by §2702, subject to its exceptions | Requires a valid legal basis and transfer ground | Requires a valid legal basis and process |
| General duty to decrypt content | None identified; order and provider classification still require review | None identified in the instruments reviewed | None identified in the sources reviewed |
| Exposure | Statute- and process-specific | Up to €20m or **4% worldwide turnover** for covered Chapter V infringements | Statute- and decision-specific |

Three consequences shape everything below.

**Türkiye is the conservative engineering constraint, conditionally.** If a production operator is
a *yer sağlayıcı* within the territorial scope of 5651 Art. 5, the statute requires retention within
a one-to-two-year band and the **accuracy, integrity and confidentiality** of that traffic data.
Encrypted, integrity-protected, access-controlled storage is therefore the product default for a TR
deployment, but counsel must confirm both classification and the current implementing period before
activation.

**The EU deadline is procedural, not technical.** Regulation (EU) 2023/1543 applies from
**18 August 2026**. It requires a designated establishment or legal representative, notified to a
member state, able to answer an 8-hour emergency order. No code satisfies this. It is a company
obligation due on 18 August 2026; as of this document's 9 August 2026 review, the deadline is
imminent and the prerequisite remains open.

**§2702 cuts the other way.** In the US, providers covered by the Stored Communications Act are
generally prohibited from voluntarily divulging covered content and records except through the
statute's enumerated exceptions. An informal request is therefore never sufficient authority. The
refusal and escalation path is part of the design, not a preference.

### 1.1 What we probably are

| Jurisdiction | Classification | Consequence |
| --- | --- | --- |
| Türkiye | Candidate *yer sağlayıcı* (hosting provider) | If confirmed: retention and notification obligations; separate analysis is required for any *sosyal ağ sağlayıcı* threshold |
| EU | Candidate hosting service and e-Evidence service provider | If confirmed: orders, contact path, designated establishment or legal representative |
| EU | ECS/NI-ICS classification unresolved | See below — this materially changes the regulatory analysis |
| US | Candidate ECS and/or RCS under the SCA; CALEA status unresolved | If confirmed: §2702 restrictions and §2703 process |

The EECC defines an interpersonal communications service by direct, interactive exchange between a
**finite number of persons**. It separately includes transmission services used for machine-to-machine
services within the broader definition of electronic communications service; it does not simply
exclude all machine-to-machine systems. Pigeonpost is an application-layer store-and-forward service
whose protocol correspondents are keypairs, so classification cannot be inferred from branding or
transport alone. Counsel confirms the EU and relevant member-state classifications before a
production retention policy is enabled.

### 1.2 The rule that dictates storage layout

In *La Quadrature du Net II* (C-470/21, Full Court, 30 April 2024), the Court's analysis of certain
IP-address retention and access arrangements depends on safeguards including a **genuinely
watertight separation** from civil identity data. The precise holding is context-specific; this
project adopts separation as a conservative engineering invariant rather than claiming that the
judgment authorises every retention programme.

That is an engineering requirement in the language of a judgment. Network data and identity data
live in different stores, under different keys, released under separate authorisations. Nothing in
the codebase joins them, and no runbook joins them in one step.

## 2. What we keep

Purpose-separated stores, deliberately never joined by an online service.

### 2.1 The sealed trace log — `loft` and identity-enabled `registry`

One network record per in-scope inbound request: standing capture on TR and US nodes, and only while
an authenticated preservation policy is active on an EU node. Records are readable only under §4.
Jurisdiction is an operator configuration backed by deployment location and legal scope; it is never
guessed by geolocating the source address.

```
NetworkTraceRecord v1 — strict versioned binary encoding
  ts_ms      u64        millisecond UTC; without this CGNAT resolution fails
  node_id    [u8; 32]   which node observed it
  juris      enum       TR | EU | US — decides which epoch it seals under
  op         enum       Publish | Fetch | PutAgent | Claim
  src_ip     IpAddr
  src_port   u16        an address without a port often resolves to nobody
  event_id   [u8; 32]?  Publish — the join key a recipient can name
  recipient  [u8; 32]?  Publish — which inbox
  owner      [u8; 32]?  Fetch — which keypair proved ownership
  size       u32?       Publish
  correlation_commitment [u8; 32]?  Claim only; random one-time join commitment

IdentityTraceRecord v1 — separate strict binary encoding and store
  ts_ms      u64
  node_id    [u8; 32]
  juris      enum
  op         Claim
  correlation_commitment [u8; 32]
  subject    String      provider subject; never an address
```

The registry writes the network and identity records to different stores under different key
purposes and custody authorisations. No record type or ordinary command contains both source address
and provider subject. The correlation commitment can be joined only by two separately authorized
offline disclosures.

Directory add/drain traffic is authenticated control-plane activity, not a message transport or
identity claim. Its loft-signed mutation and witnessed registry leaf are retained, while its source
address is used only in bounded in-memory admission and is not persisted. `NetworkTraceRecord v1`
has no directory operation; bringing those requests into legal-retention scope would require an
explicit policy decision and a new versioned operation rather than an undocumented reuse of
`Claim`.

**Never in either trace record:** plaintext, wrap ciphertext, rumor, seal, capability tokens, PoW
nonces, or anything derived from message content. The schemas have no field content could occupy,
and that is load-bearing — see §5.

`event_id` is what makes this work without touching encryption. A wrap's id is a deterministic hash
of the wrap, so a recipient can compute the id of a message they decrypted and complained about. Law
enforcement arrives with an id; we answer which address published it, and when. The sender is
identified by network origin although the loft never saw who they were.

### 2.2 Retention, per jurisdiction

| Node | Retention | Basis |
| --- | --- | --- |
| **Türkiye** | Configured only after counsel confirms the current period; never below the statutory minimum | Conditional 5651 Art. 5 path |
| **United States** | 30 days | Product abuse-response choice, not a mandate; extended only by authenticated preservation process |
| **European Union** | Preservation only by default | Product minimisation choice; member-state law and authenticated orders can alter a deployment policy |

An EU deployment never retains "to be helpful." Every retention purpose, legal basis, scope, and
period must be documented under GDPR principles and applicable member-state law.

The offline inventory makes that review a fail-closed configuration decision. Retention-policy v1
encodes the 30-day US product choice, zero standing days for the EU, and counsel's selected Türkiye
period within 365–730 days, together with a nonzero commitment to the approval record. The choice is
stored in every epoch inventory and must match the epoch's private operator configuration; changing
it uses the offline inventory-update ceremony and does not require a code release. An update cannot
shorten an epoch's already-computed retention, and every policy revision requires a new approval
commitment. There is deliberately no hard-coded Türkiye default. Legacy inventory state that did
not record the decision is rejected and must go through the documented inventory ceremony before
production use.

## 3. Sender attribution — what we chose to add

> [!WARNING]
> **This is a choice, not a legal requirement.** No US, EU, or Turkish instrument requires us to be
> able to identify a sender. §2 already satisfies every duty in §1. What follows converts a technical
> impossibility into a standing capability, answerable to every jurisdiction that ever asks, for
> every message in the retention window. It is recorded here as a decision so that it is never
> mistaken for something the law compelled.

`architecture.md` says the wrap "carries no link to the sender." From envelope v3 that is true of
the loft and false of the network as a whole, and both documents must say so.

### 3.1 The two failure modes of the obvious design

The obvious design — put the sender in the envelope, encrypt it to one master key we hold — fails
twice, and both failures are fatal rather than inconvenient.

**A master key deanonymizes retroactively.** One compromise, one insider, one hostile subpoena of
the custodian, and every sender of every stored message on every loft — including third-party lofts
and backups — is exposed at once, permanently. The blast radius is the entire history of the network.

**Client-side escrow is unverifiable by the server.** A loft cannot decrypt the attribution block,
so it cannot tell a genuine one from 200 random bytes. A patched client emits well-formed garbage and
every loft accepts it. Escrow that only honest senders populate is worse than none, because we would
report having it.

### 3.2 The construction

Both are fixed by moving the key to an epoch and requiring a sender signature that the recipient and
the offline custodian can verify independently.

**Compliance keys are per-epoch and published.** An X25519 keypair `(S_c^e, P_c^e)` per calendar
month. Public keys are published in the registry's transparency log, so an operator cannot hand one
agent a different key from everyone else — targeting requires forking the log, which is exactly the
attack the log already exists to catch. Private keys live in custody per §4, never on a loft.

**The block, built by the sending client inside envelope v3:**

```
e_sk, e_pk  fresh X25519 keypair, per message
key_id      canonical { version, purpose=Attribution, jurisdiction, authority,
                        epoch_start_ms, generation }
key_digest  SHA-256(P_c^epoch)
shared      X25519(e_sk, P_c^epoch)
k_sab       HKDF-SHA256(shared, info "pigeonpost/envelope/v3/attribution",
                        salt e_pk ‖ key_digest)
event_id    SHA-256(v3 event-id domain ‖ signed outer core fields)
sig_input   domain ‖ block_version ‖ key_id ‖ key_digest ‖ e_pk ‖ event_id
                   ‖ recipient_pubkey ‖ sent_at_ms
sender_sig  Ed25519(sender, sig_input)
plain       sender_pubkey[32] ‖ sent_at_ms:u64 ‖ sender_sig[64]       # exactly 104 bytes
aad         key_id ‖ key_digest ‖ e_pk ‖ event_id ‖ recipient_pubkey
sab         XChaCha20-Poly1305(k_sab).seal(nonce, plain, aad)         # exactly 120 bytes

wrap.attribution = { version, key_id, key_digest, e_pk, nonce, ct }
```

The v3 event id excludes the outer signature, PoW nonce, and attribution block, while the ephemeral
wrap signature covers every id field plus the complete block. The sender first puts `e_sk` in the
sender-signed seal and encrypts that seal into the outer ciphertext; the v3 event id is then fixed;
only then is the attribution claim signed and encrypted. The block can therefore bind directly to
the trace join key without circularity. Lifting it to another event or recipient, changing its key,
or stripping it fails a signature or AEAD check.

The calendar-month key is selected and verified against the sender-signed true `sent_at_ms`, not
the public wrap's privacy-jittered `created_at`. Backward jitter may legitimately put that visible
timestamp in the preceding month; it only supplies the lower and maximum-jitter bounds for the
signed claim.

**The seal carries `e_sk`.** The seal is readable only by the recipient and signed by the sender.
Putting the ephemeral secret there is what makes the whole scheme enforceable:

1. Recipient opens the wrap, opens the seal, and learns the true sender from the seal's signature.
   The seal signature covers `e_sk`, so a sender cannot be handed a secret it never authorised.
2. Recipient reads `e_sk` from the seal and fetches `P_c^epoch` **independently** from the log.
   Verifying against a compliance key the *message* supplied would let a sender escrow to a key
   nobody holds — the whole failure this design closes.
3. Recipient recomputes `shared`, derives `k_sab`, and reconstructs the AAD entirely from the wrap
   plus trusted registry key history.
4. Recipient decrypts the fixed claim; verifies `sender_sig`, exact event/recipient/key binding,
   timestamp bounds, and equality to the sender who signed the seal.

The custodian follows the same verification with `S_c^epoch` and `e_pk`. It needs no
recipient-encrypted seal fields: event id, recipient, key id/digest, and `e_pk` are public in the
wrap, while the decrypted claim carries the sender key and its signature. This property is required
before any attribution result may be disclosed.

Any mismatch — wrong compliance key, wrong sender, copied block — and the attribution result is
**Invalid**. A missing block is a *different fact* (`Absent`) from a failed one, and callers must be able
to tell them apart: a forged block is an attempt to look compliant, which is a stronger adverse
signal than no block at all. The recipient learns nothing it did not already know from the seal, so this leaks
nothing; but nobody has to trust the sender, which is the entire point.

**The recipient selects the custodian scope.** One signed fixed `AttributionRequirement` pairs the
jurisdiction with the stable 32-byte authority/custodian id from `key_id`; monthly epoch and key
generation remain Registry-selected. The exact value appears in AgentRecord v2 for discovery and
RecipientPolicy v3 for admission. A sender must explicitly agree to that exact public requirement;
resolving it is not consent, and the sender cannot substitute another jurisdiction or authority.
The all-zero authority is invalid. Enabling a requirement fails unless a fresh threshold-witnessed
Registry view already contains a matching `Active` key.

**Enforcement splits, exactly as spam does.** The Loft checks presence and public structure — known
typed key, exact match to the recipient-signed jurisdiction and authority, matching digest, correct
fixed lengths, epoch currently valid — and accepts a newly published attributed wrap only while
that key is `Active`. This Active-only rule also applies to voluntary attribution where omission is
allowed. A `Retired` key remains available only for historical recipient/custodian verification of
wraps already created; a `Revoked` key is never usable. Correctness is enforced client-side after
unwrap, the same division `spam.md` already uses, and for the same reason: the Loft cannot decrypt
or authenticate the sender claim. Observed request-source metadata is separately sealed and is not
a sender key. Temporary resolver/cache unavailability is distinct from invalidity: an older
witnessed prefix cannot prove that a matching key was not appended later. Loft admission returns a
retryable unavailable result, and a required recipient leaves that Loft cursor unchanged until a
consistency-verified refresh succeeds; explicit scope, cryptographic, `Retired`, or `Revoked`
admission failures remain terminal.

Signed wraps are immutable. If an outbox copy reaches a Loft only after its key retires, admission
is terminal and the client must create a new explicitly authorized send under the current active
key; it never rewrites or silently re-escrows the old ciphertext. A recipient scope change also
applies immediately to unread fetched wraps, so operators drain before changing authority when they
intend to accept delayed traffic from the superseded scope.
Attributed v2 is always `Invalid`; it is read-compatible only so old message content is not lost.

### 3.3 What this does not achieve

Stated here so that nobody oversells it to a regulator.

- **A determined sender simply omits it.** A patched client sends an unattributed message. Recipients who
  require attribution reject it; recipients who do not, accept it. Attribution works where the
  recipient demanded it, and nowhere else.
- **It is escrow, and escrow's central risk survives.** Holding `S_c^e` decrypts every block from
  that month that anyone possesses. Monthly epochs and per-event disclosure bound it. They do not
  remove it.
- **It weakens requirement 6.** "Not controlled by us once adopted" is less true than it was. A fork
  that strips attribution is a legitimate fork, and we should expect one.

## 4. Custody, disclosure, and destruction

The same primitive family serves network traces, identity traces, and attribution blocks, with a
**different typed key id, epoch stream, store, authorization, and custody inventory for each** per
§1.2. A network-trace command cannot request an identity key, and no ordinary command opens both.

**Sealing.** One key per day per jurisdiction for trace records; one per month for attribution.
Records are sealed under the epoch key; the epoch key is wrapped to the offline compliance public
key. A production trace writer necessarily holds its current daily symmetric sealing key for crash
recovery, zeroizes it at rollover, and cannot unwrap a closed epoch because it holds no compliance
private key. A full compromise of a correctly configured loft therefore exposes at most the open
day's trace key and no attribution private key.

**Custody.** `S_c` never touches a node. The offline binary calls an explicitly configured process
adapter; production must provision that adapter against a segregated KMS/HSM with two-person
approval or an independently administered *k*-of-*n* ceremony. The repository's software custody
backend is test-only and is not a production KMS or Shamir implementation. Exporting personal data
or associated custody material from Türkiye must be reviewed under the KVKK cross-border transfer
framework amended in 2024. Türkiye-resident custody is a conservative deployment choice, not a
claim that the statute categorically requires every key to remain in-country.

**Disclosure.** Order arrives → authenticate and validate against §6 → append a disclosure-intent
record → obtain the purpose-specific quorum → unwrap only named epochs → decrypt and verify only the
records or blocks selected by the order → append completion/failure. Never bulk decryption, never a
standing decrypted copy, never a key handed to a requester.

**The disclosure log uses the registry's exact RFC 6962 Merkle construction in an independent
offline ledger.** Public intent/completion leaves contain timestamp, jurisdiction, purpose, epochs
touched, result count, and salted commitments to order reference, selectors, requester, and
approver. Raw values live only in a separately encrypted private audit record within
Pigeonpost-controlled persistence; they enter the offline operator transiently through bounded
stdin from the separately protected case-management boundary and are rejected in argv and
environment variables. Publishing them would itself disclose identity/network data and escape
retention. The ledger is verified as a bounded stream, maintains an incremental checkpoint root,
and can reproduce public inclusion and consistency proofs without retaining its records in memory.
The offline operator verifies the retained signed handoff as a monotonic RFC 6962 floor before each
ledger operation, establishes an empty floor before the first intent, and advances it before any
disclosure bytes leave the process. A missing floor for a nonempty ledger or a newer, conflicting,
wrongly signed, or inconsistent floor fails closed. It atomically writes each accepted checkpoint
to an owner-only publication handoff.
A separately provisioned and monitored publisher must expose that signed note on the same cadence
as the names log, without receiving the signing key or private state. This makes the transparency
report externally verifiable without making the order searchable; scheduler provisioning and
monitoring remain explicit deployment evidence rather than a source-code claim. Coordinated
rollback of the ledger, local sidecar, and handoff is detectable only against that independently
retained public history, so production activation requires the publisher/monitor to reject a signed
head regression or fork.

**Destruction is by every key copy, not one file.** At end of retention, the scheduler proves that
no active hold applies and deletes every inventoried custody copy: live metadata, SQLite WAL and
sidecars, snapshots/backups, KMS versions, or Shamir shares. Only then may it record completion. The
segment becomes permanently unreadable whether or not its ciphertext bytes remain. The same applies
to `S_c^e`: destroying every copy of a month's attribution secret makes those blocks undecryptable.
Deleting a database row while a backup can still unwrap is not cryptographic erasure. A producer-
signed terminal manifest is still mandatory, but missing or corrupt ciphertext cannot be allowed to
retain its decryption key indefinitely: the operator durably records the manifest commitment and
integrity degradation before destroying every copy. Disclosure of the same incomplete bundle
remains fail-closed.

A single transactional legal-hold state machine pins an epoch key against expiry. Place, renew, and
release each require two distinct pinned approvers; renewals persist their predecessor id and
releases name the exact hold id. A §2703(f) preservation request is a renewable 90-day hold; expiry
and shred acquire the same lock/state transition so they cannot race. Shredding evaluates each
epoch independently, resumes durable partial destruction, and does not let a held or failed epoch
block another eligible epoch.

## 5. What we will not build

Each of these is a product boundary and a default escalation rule. Counsel decides whether a valid
order requires a different legal response; the software still cannot produce plaintext or keys it
does not possess.

- **No content decryption capability.** Pigeonpost nodes do not possess recipient content keys.
  CALEA distinguishes telecommunications carriers from information services, which include
  software-based electronic messaging, but final classification remains counsel's work. The July
  2026 EU interim voluntary-detection measure excludes number-independent interpersonal
  communications to which end-to-end encryption is, has been, or will be applied; that narrow rule
  is not a general conclusion about every future instrument. The attribution scheme remains
  purpose-limited: separate keys, a separate store, and a fixed plaintext schema with no content
  field.
- **No voluntary disclosure to US government without process** (§2702). The only routine exception
  is a good-faith emergency involving danger of death or serious physical injury — which *permits*
  disclosure and never compels it.
- **No direct answer by default to a non-EU order for EU-held data** (GDPR Art. 48 and final EDPB
  Guidelines 02/2024). Route it to counsel for an applicable international agreement and Chapter V
  transfer analysis. Article 48 is not permission to treat every direct request alike; exceptional
  grounds are assessed case by case. Covered Chapter V infringements can fall within the Article 83
  tier of €20 million or 4% of worldwide annual turnover, whichever is higher.
- **No observed client/source IP address in any application log.** The sealed store is the only
  place an observed network address may land. In stderr, a crash report, an APM trace, or a reverse-proxy access log it sits outside the
  retention timer, outside custody, outside the audit log — and §1.2 collapses. This includes the
  proxy an operator puts in front of their loft, which is where it will happen by accident.
  An endpoint or bind value explicitly requested in a local operator command is configuration
  output, not access telemetry; it must still never be copied into ordinary application logs.
- **No master key.** See §3.1.

## 6. Operating this

**Intake.** One published address for legal process; no other path is valid. Every order is
authenticated before it is actioned — an attached document claiming to be a court order is an
untrusted request until verified against the issuing authority.

**By 18 August 2026.** A designated establishment or EU legal representative, notified to a member
state, with declared accepted languages, and a human reachable inside 8 hours. The company obligation
is independent of anything in this repository.

**Deferred (2026-08-12, owner decision).** No EU representative is designated yet. Pigeonpost is not
marketing to or targeting users in the Union at launch; **an EU representative will be designated when
we serve EU users** and the obligation is triggered. This is a deliberate, owner-accepted deferral of
a company obligation — recorded here, not satisfied. Nothing in this repository asserts the
designation exists: the release-authorization designation-evidence digest
(`PIGEONPOST_EU_EVIDENCE_DESIGNATION_SHA256`) remains intentionally unset until a real designation is
in place.

**Scope, stated every time.** A message published to three in-scope lofts leaves a record at each of
them. We can answer only for the subset of nodes we run. Every production response says so in
writing — a fragment that reads as complete is how a compliance process becomes an obstruction
allegation.

**Operators may inherit duties, and must be told.** Running a public node in or serving Türkiye may
trigger *yer sağlayıcı* or other obligations depending on the operator, service, and territorial
scope. `install` prints a jurisdiction notice and `node.md` carries the detail. The sealed trace log
ships as something an operator can configure after legal review, not something they must build — an
operator who cannot comply should learn that before activation, not after.

## 7. Consequences for the design docs

| Document | Claim that changes |
| --- | --- |
| `architecture.md` | "The wrap carries no link to the sender" — true of the loft, false of the network from envelope v3 |
| `product.md` | Requirement 5 ("sender and recipient only") gains an escrow carve-out; requirement 6 is materially weakened (§3.3) |
| `spam.md` | Attribution is a fifth layer and a new `RecipientPolicy` field |
| `network.md` | Threat model gains the compliance-key custodian as an actor |
| `node.md` | Jurisdiction notice at install; retention and custody are operator obligations |
| `keys.md` | Unchanged. Attribution binds the identity key, it does not touch rotation |

## 8. Open — counsel decides, not us

Ordered by how much design each answer moves.

1. **Is agent-to-agent messaging an ECS/NI-ICS in the EU**, given the EECC exclusion of
   machine-to-machine communication? Decides whether any EU retention mandate exists at all.
2. **Which member state hosts the e-Evidence designation**, and can the same entity serve as DSA
   Art. 13 legal representative? Due now.
3. **Does 5651 reach a loft operated outside Türkiye** serving Turkish agents, or only one hosted
   in-country? Decides whether a TR-resident node is required.
4. **The current regulation figure** for hosting-provider traffic retention — one year, or something
   else after recent amendments? Sources differ; the statute sets a 1–2 year band.
5. **Does running the directory make us the provider of the network** rather than of our own nodes?
   Decides whether compliance scope is three servers or the whole pool.
6. **Is a free, non-commercial service an "information society service"** for e-Evidence and DSA?
   Probably unavailable as an escape; worth confirming rather than assuming.
7. **Can the registry's public log carry a salted identity commitment** instead of a cleartext
   subject, and still discharge its transparency purpose? Decides whether the erasure-rights problem
   is fixable without losing auditability.

## 9. Primary sources checked

Checked 2026-08-08. Links are inputs to counsel review, not substitutes for it.

- US Code: [18 U.S.C. § 2702](https://uscode.house.gov/view.xhtml?edition=prelim&num=0&req=granuleid%3AUSC-prelim-title18-section2702), [18 U.S.C. § 2703](https://uscode.house.gov/view.xhtml?edition=prelim&num=0&req=granuleid%3AUSC-prelim-title18-section2703), and [47 U.S.C. § 1001](https://uscode.house.gov/view.xhtml?edition=prelim&num=0&req=granuleid%3AUSC-prelim-title47-section1001)
- EU: [Regulation (EU) 2023/1543](https://eur-lex.europa.eu/legal-content/EN/TXT/?qid=1659727831419&uri=CELEX%3A32023R1543), [Directive (EU) 2023/1544](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX%3A32023L1544), [European Electronic Communications Code](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX%3A02018L1972-20181217), [GDPR](https://eur-lex.europa.eu/eli/reg/2016/679/oj), and [EDPB Guidelines 02/2024, final version](https://www.edpb.europa.eu/documents/guideline/guidelines-022024-on-article-48-gdpr_en)
- EU July 2026 interim measure: [Council final approval summary](https://www.consilium.europa.eu/en/press/press-releases/2026/07/23/fighting-child-sexual-abuse-online-interim-measure-protecting-children-to-be-reinstated/)
- Türkiye: [Law No. 5651 official legislation record](https://www.mevzuat.gov.tr/mevzuat?MevzuatNo=5651&MevzuatTertip=5&MevzuatTur=1) and [KVKK cross-border transfer guidance](https://www.kvkk.gov.tr/Icerik/2053/Yurtdisina-Aktarim)

## Workspace context is personal data

Added 2026-08-14 with the handle-namespace work.

A mailbox may record what it works on: git repository, job title and description, machine name, and
the full local path of a checkout such as `/Users/bekir/Documents/apps/generic`. Machine names and
home-directory paths routinely contain a person's name, so this is personal data under GDPR Art. 4
even though no field asks for one.

Two facts that shape the obligations:

- **It is client-encrypted.** The postbox stores a nonce, ciphertext and salt and holds no key, so
  we cannot read it, disclose it on request, or hand it to a processor. Erasure and export are
  still ours to provide, and both are satisfied by the ciphertext: deleting the mailbox deletes the
  context with it (enforced in the same transaction, with a test), and export returns the blob the
  owner can decrypt.
- **A breach discloses less than the same table in plaintext would.** An attacker with the database
  learns that a mailbox has context and roughly its size. That is a materially smaller notification
  than "we lost a map of which repositories live at which paths on which machines".

Retention follows the mailbox: no separate clock, nothing to forget to sweep.

The deferred EU-representative designation (above) becomes more pressing rather than less. Holding
this category of data for EU users without a designated representative is a gap to close before
handle sales open, not after.