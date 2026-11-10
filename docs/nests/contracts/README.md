# Nests marketplace core contracts (proposed)

**Status:** design contracts for the Nests marketplace build plan. Operations marked
`x-pigeonpost-status: implemented` are served by the postbox (`crates/pigeonpost-postbox/src/marketplace.rs`
for M0, `marketplace_m1.rs` for M1) behind the per-account `marketplace` flag, off by default, and
none of them is deployed. The other operations are proposed only. See "Marketplace M0" and
"Marketplace M1" below.

These files are the aligned version of the core contracts in the owner's external marketplace
expansion (v5, 2026-11-03). v5 was written without full knowledge of the current system; this
directory keeps its intent and changes its shape to match Pigeonpost's conventions, principals and
naming. Every change is listed below.

| File | What it is |
|---|---|
| `pigeonpost-nests.v1.yaml` | OpenAPI 3.1 source of truth: four operations, shared error envelope, schemas |
| `pigeonpost-nests.v1.json` | JSON mirror, written by `.github/scripts/openapi/export_json.py` |
| `schemas/nest-job-request.v1.schema.json` | Standalone JSON Schema (2020-12) of the job request |
| `schemas/nest-payment-authorization-request.v1.schema.json` | Standalone schema of the payment authorization request |
| `schemas/nest-review-status.v1.schema.json` | Standalone schema of the publisher-visible review status |
| `examples/*.json` | Synthetic examples; every identifier is made up |

A schema checks structure only. It does not establish identity, funding, review state,
revocation, time ordering or token entropy. Those are decided by `authorize(actor, action,
resource)` in `crates/pigeonpost-postbox/src/policy.rs` and by the financial authority.

## Operations

| Method and path | Purpose | Credentials |
|---|---|---|
| `POST /v1/nest-jobs` | Submit one job to a Nest operation | member token, account key, or the acting mailbox's capability token; never a connector or an org key |
| `GET /v1/nest-jobs/{job}` | Read one job's admission, execution and settlement state | same |
| `POST /v1/nest-payment-authorizations` | Reserve credits for one quote: exact human approval, or consumption of a spend mandate | a person for human approval (step-up above a threshold); automation only with `mandate_id` |
| `GET /v1/publishers/{publisher}/reviews/{review}` | Redacted status of a release candidate's review | the publisher's account, or org members holding the publisher role |

## Divergences from v5

### Files and layout

1. **Location and names.** v5's `pp-nest-core.openapi.yaml` and `pp-*.v1.schema.json` become
   `pigeonpost-nests.v1.yaml` and `schemas/nest-*.v1.schema.json`. The repository names specs
   `pigeonpost[-area].v1.yaml`, and `pp` is not an abbreviation used in our APIs (`/pp` is an open
   handle namespace).
2. **One source, derived schemas.** v5's OpenAPI referenced the schema files by relative path.
   Here the schemas live in `components.schemas` of the OpenAPI file, so the repository's validator
   (which has no base URI for relative references) can check the whole contract, and the
   standalone files are derived from them (`#/components/schemas/X` becomes `#/$defs/X`).
3. **JSON mirror.** Every spec in this repository has a JSON mirror checked by
   `check_openapi.py`; this one does too.
4. **Schema ids.** `$id` values are `urn:pigeonpost:schema:nest-…:1` (v5 used the same scheme with
   different names).
5. **Not carried over:** v5's `SPECIFICATION_VALIDATION_REPORT.md` (replaced by the validation note
   below) and its examples (replaced by synthetic ones that use `/example/...` addresses).

### Conventions

6. **Envelopes.** Success bodies are `{"data": …}` and errors are
   `{"error": {"code", "message", "doc_url", "details?"}}`, as in the Enterprise API
   (`openapi/pigeonpost-org.v1.yaml`). v5 returned bare objects and an error with only
   `code` and `message`. The older `/v1` routes use a flat `{"error": "<code>"}`; new marketplace
   routes follow the newer org convention (an open decision in the build plan, recommended).
7. **Idempotency.** v5's body field `idempotency_key` becomes the `Idempotency-Key` header, which is
   **required** on both `POST`s (16-128 characters, v5's pattern). Unlike ordinary org API replays
   (24 h), these keys are kept for the life of the job or financial record, as v5 requires.
8. **`request_id` removed.** The `Idempotency-Key`, the server's `job_id` and the optional
   `client_trace_id` already cover what it did; two client ids for one operation invite
   disagreement.
9. **Identifiers.** Server-minted ids use the repository's `<prefix>_<26 lowercase base32>` form
   (as `enr_…`, `grp_…`, `wh_…`). Prefixes: `job_`, `qte_`, `rsv_`, `rel_`, `pub_`; v5's `revw_`
   is `rvw_` and `mand_` is `mnd_`.
10. **Payment authorization format.** v5's `payment_authorization_ref` (`payauth_` plus at least
    one character) becomes `payment_authorization` with at least 43 base64url characters, which
    is the 256 random bits v5 asks for, so a malformed or truncated reference fails structurally.
11. **Errors.** v5's error families are kept where they name a real outcome. `feature_deferred`
    is `feature_disabled` (`404`), which is how every flagged route here already answers. Added the
    existing policy codes `credential_not_accepted`, `human_required` and `step_up_required`, plus
    `quote_expired`, `mandate_limit`, `access_expired` and `invalid_operation_payload`. Codes v5
    listed for routes outside this contract (`review_blocked`, `artifact_mismatch`,
    `source_evidence_required`, `configuration_outside_policy`, `outcome_unknown`,
    `sandbox_budget_exhausted`, `project_funding_unavailable`) belong to those routes' contracts.
12. **Servers.** v5 named no server. This file names the public postbox host as the proposed
    placement, consistent with the other specs, and marks it `x-pigeonpost-status: proposed`.

### Authentication and authority

13. **Credentials.** v5's single generic `bearerAuth` becomes the three existing schemes: member
    token, account API key and mailbox capability token. Org API keys (`pk_org_…`) are not accepted:
    in our model they automate one organization's administration and cannot spend, read content
    or act as a mailbox. Connector tokens are refused.
14. **Acting mailbox.** `x-pigeonpost-identity` is kept (it is the existing `/v1` header) but is
    optional: a capability token always acts as its own mailbox, as on every other route.
15. **Mandates.** Automated use of a spend mandate is the policy action `ConsumeSpendMandate`,
    separate from the never-auto `spend` verb, which is unchanged. The verb list in `verbs.rs` and
    the agentd routing rules are not touched.

### Job request

16. **`type`** is `nest.job.request` (v5: `pp.nest.job.request`), in the dotted style of our event
    types.
17. **`capability` / `capability_version` are `operation` / `operation_version`.** "Capability"
    already means the mailbox capability token (`cap_…`) and `GET /v1/capabilities` (feature
    flags). `operation` is lower-case dotted words; `operation_version` is `major.minor[.patch]`.
18. **`reply_to_identity_id` is `reply_to`,** a mailbox address (our APIs name mailboxes by
    address), optional and defaulting to the acting mailbox.
19. **`attachments`** is a new top-level list. v5 put attachment references inside `payload`; at the
    top level the platform can bind their content digests into the intent and check their audience
    before the operation's own schema sees them.
20. **Unchanged:** `to`, `quote_id`, `payload` (an object, validated against the pinned operation
    schema), `deadline_at`, `client_trace_id`, the 256 KiB / depth 32 limits, rejection of duplicate
    keys and non-finite numbers, and rejection of server-owned fields (`from`, `payer`,
    `amount_minor`, `customer_id`, `approval` and others).

### Responses

21. **`GET /v1/nest-jobs/{job}` added.** v5 named the route in its route list but gave no schema.
    `NestJob` carries v5's independent admission, execution and settlement states and a count of
    external effects whose outcome is unknown.
22. **Payment authorization response** keeps v5's fields (`amount_minor`, `currency`,
    `reservation_id`, `expires_at`) inside `data`, and adds `Cache-Control: no-store`.

### Review status

23. **Path.** `/v1/developer/reviews/{review_id}` becomes
    `/v1/publishers/{publisher}/reviews/{review}`. The owner (a personal account or an
    organization) is explicit, as in `/v1/orgs/{id}/…`, and "developers" stays the name of the
    developer documentation site.
24. **Fields.** Added `publisher_id` and `previous_review_id` (a resubmission is a new review that
    points to the old one). Kept the six fixed stages, their order, statuses, attempts, timestamps
    and the conditional rules (approved needs decision and approval references; rejected or
    changes requested needs decision and findings; failed, blocked or skipped needs a reason).
25. **Action hints.** v5's `deploy` hint is removed and `publish` is `publish_listing`. `deploy` is a
    never-auto verb in Pigeonpost, and v5 itself separates listing publication from deployment;
    installation is the buyer's action, not the publisher's.

## Keeping the files in step

After editing the YAML:

1. regenerate the mirror: `python .github/scripts/openapi/export_json.py docs/nests/contracts/pigeonpost-nests.v1.yaml`;
2. regenerate each standalone schema from its component (`NestJobRequest`,
   `NestPaymentAuthorizationRequest`, `NestReviewStatus` plus its `NestReviewStage`), rewriting
   `#/components/schemas/` to `#/$defs/` and adding `$schema` and `$id`;
3. validate: `python .github/scripts/openapi/check_openapi.py docs/nests/contracts/pigeonpost-nests.v1.yaml`.

The CI `openapi` job validates this file along with the two production specs. A postbox test
(`marketplace_tests::the_routes_and_the_contract_match`) fails when a route in `marketplace.rs` or
`marketplace_m1.rs` and an operation marked implemented stop matching, or when an operation names a
policy action its handlers do not use.

## Marketplace M0: what is built and how it departs from the plan

All routes are behind the per-account `marketplace` flag. The flag is off by default. When off, every route returns 404 feature_disabled. No marketplace code is deployed.

### Routes and Policies

| Route | Policy Action | Who Can Access |
| :--- | :--- | :--- |
| GET /v1/staff/roles<br>POST /v1/staff/roles<br>DELETE /v1/staff/roles/{role} | StaffRolesManage | Platform owner only. A person with step-up. Owner is set in env `POSTBOX_PLATFORM_OWNER_ACCOUNT`. If unset, nobody has access. |
| POST /v1/publisher-applications | PublisherApply | A person for their account, or an org owner/admin for the org. |
| GET /v1/publisher-applications/{application} | PublisherManage | The applicant account, or org owner/admin. Others get 404. |
| GET /v1/staff/publisher-applications/{application} | NestReview | A person with a live reviewer role and step-up. Must have no stake in the publisher. Returns 403 staff_conflict if they have a stake. Returns 403 staff_required if not staff. |
| GET /v1/marketplace/listings/{listing} | NestDiscover | Anyone, for published public or unlisted listings. The publisher for its own listings. Others get 404. |

### New Policy Actions and Requirements

New actions: NestOperate, NestDiscover, NestInstall, NestConfigure, NestManage, NestInvoke, NestMarketplacePurchase, PublisherApply, PublisherManage, NestPublish, NestReview, NestAttest, NestQuarantine, PayoutPrepare, PayoutApprove, StaffRolesManage.

New requirement: `Requirement::Staff`. This requires a person holding the live staff role named by the action (reviewer or finance) and step-up authentication.

### Deviations from the Build Plan

Owner decisions from 2026-11-09. These are not to be reopened.

1. **Host isolation**: Uses nests-1 only. No KVM, no Firecracker. Marketplace code runs under gVisor as a separate pool behind the Sandbox trait. One sandbox per buyer context. No shared mounts between contexts. Network access is limited to the broker. The plan (R-51, R-66, section 9) called for Firecracker on a KVM host. gVisor is a weaker boundary, as stated in phase-a-plan.md.
2. **Payments**: No wallet or stored credit balance. Per-purchase payment is used instead. CreditsTopUp, CreateSpendMandate, RevokeSpendMandate, ConsumeSpendMandate, and Requirement::MandateHolder are not added. NestMarketplacePurchase (for a person) replaces them. The proposed /v1/nest-payment-authorizations operation still describes wallets and mandates but is not implemented; it will be reworked with payments in M3.
3. **Reviewer roles**: The owner is the only reviewer. Staff remains a principal kind, but review does not require two named people. The separate preparer/approver rule for payouts stays in the design but is not in M0 code.
4. **Seller of record**: Piyote Bilişim Ltd. Commission, reserve, and payout values never appear in tracked files.
5. **Infrastructure**: The nestd ledger stays on SQLite instead of the plan's PostgreSQL. OpenBao and SeaweedFS are skipped for now (one host).
6. **Models**: Self-hosted only. No third-party model route is added for marketplace work.
7. **Flag name**: Uses `marketplace` per account, instead of this README's earlier `nests_marketplace`.
8. **Database schema**: The `nests` table does not have an owner_kind column. Org-owned Nests are already identified through orgs.internal_account_id, so the column would duplicate a fact that can drift. New tables that can belong to a person or an org carry owner_kind and owner_id.

## Marketplace M1: the reference path

M1 builds a first-party reference release (HR policy Q&A, `deploy/nests/reference/hr-policy-qa`) from a pinned source tree, analyses it, has staff approve and attest it, and serves native jobs to many buyers from one shared listing, one worker per buyer context. Everything is behind the per-account `marketplace` flag, off by default; the internal routes also need it on for the platform owner's account (`POSTBOX_PLATFORM_OWNER_ACCOUNT`). Nothing is deployed. No money moves: every quote and payment authorization is zero-priced, and the schema refuses any other amount.

### Routes and policies

| Route | Policy action | Who |
| :--- | :--- | :--- |
| POST /v1/nest-jobs/quote | NestInvoke | a person, an account key or a mailbox capability token; never a connector |
| POST /v1/nest-payment-authorizations | NestInvoke | same; zero-priced, single use, five minutes, required Idempotency-Key |
| POST /v1/nest-jobs | NestInvoke | same, with the quote's authorization and a required Idempotency-Key |
| GET /v1/nest-jobs/{job} | NestJobRead (new, NotConnector) | the submitting account only; others get 404 |
| GET /v1/staff/reviews/{review} | NestReview | a reviewer with step-up and no stake in the publisher |
| POST /v1/staff/reviews/{review}/decisions | NestReview, and NestAttest for an approval | same |
| POST /v1/staff/releases/{release}/security | NestQuarantine | same; actions quarantine, lift, revoke |
| POST /internal/v1/marketplace/builds | ops service token | the build plane |
| POST /internal/v1/marketplace/reviews/{review}/stages | ops service token | the analysis plane |

### How a release gets to a job

1. `pigeonpost-nest-release build` vendors dependencies in a fetch container on a fetch-only network, then runs a rootless `podman build --network=none --pull=never --no-cache --cap-drop=ALL` under runsc or crun. It writes a CycloneDX 1.5 SBOM from Cargo.lock and the file list, and one release digest over the source, vendored sources, manifest, SBOM and image digests.
2. The build record goes to POST /internal/v1/marketplace/builds. The postbox checks every digest, records the release under Pigeonpost's own publisher (owner kind `platform`), opens a review case, records the intake stage and creates the listing as a draft.
3. `pigeonpost-nest-release scan` runs the offline static scans (secrets, capabilities, syscalls, egress and external writes in the manifest, every locked package's licence against an allowlist). The behaviour test plants honeytokens and searches every channel out of the release, plain and encoded. `pigeonpost-nest-release ai-review` asks the self-hosted model `qwen-latest-flash-next` and nothing else; its decision is always `advisory`. Each stage is a review_decisions row with its evidence and evidence digest.
4. A staff reviewer with step-up approves over the exact release digest. The approval needs intake, scan and behaviour passed and the AI review recorded. It writes the attestation (canonical JSON statement and its digest) to release_attestations, and publishes the listing with its operations.
5. A buyer quotes, authorizes and submits. Admission checks the listing, the attestation, the security state, the operation's input fields, the quote and the single-use authorization. The job is admitted with execution `queued` and settlement `none`.
6. The postbox dispatcher hands the job, with the attestation statement, to nestd (POST /v1/mp/jobs). nestd verifies the statement, starts or reuses the worker for that listing and buyer context in the marketplace pool, writes the job to the worker's own volume and reads the result back from it. The worker's only network peer is the broker.
7. Quarantine or revocation stops running jobs (`stopped_security`), refuses new ones, records a release security event and tells nestd, which stops the release's workers and fences their broker tokens. Revocation is permanent, in the postbox and in nestd.

### Exit tests

| ID | What it shows | Where |
| :--- | :--- | :--- |
| SEC-01 | staff approve and attest a first-party release over its digest, with step-up; third-party publishers keep the conflict rule | marketplace_m1_tests.rs staff_approve_and_attest_a_first_party_release, third_party_publishers_keep_the_conflict_rule; store/marketplace.rs first_party_releases_may_be_reviewed_by_staff_and_third_parties_keep_the_rule |
| SEC-02 | the honeytoken behaviour test passes the reference release, catches an exfiltrating one, and a failed stage blocks the review | nests tests/marketplace_m1.rs; marketplace_m1_tests.rs analysis_stages_are_honest_and_a_failure_blocks_the_review |
| SEC-05 | quarantine stops jobs and workers, lift works, revocation is permanent | marketplace_m1_tests.rs quarantine_and_revocation; nests tests/marketplace_m1.rs quarantine_stops_workers_and_revocation_is_permanent |
| JOB-01 | quote, authorize, submit, dispatch, collect | a_job_runs_end_to_end_on_the_shared_listing |
| JOB-02 | Idempotency-Key required, exact replays, conflict on reuse | idempotency_keys_are_required_and_bind_the_request |
| JOB-03 | zero-priced, single-use, bound authorizations; no mandates | payment_authorizations_are_single_use_and_bound |
| JOB-04 | strict requests: server-owned fields, duplicates, depth, size, payload, deadline | job_requests_are_strict; nests a_resubmitted_job_is_one_job (dedupe at the host) |
| JOB-05 | agents carry jobs with their own token; a job is its submitter's | jobs_are_for_their_submitter_and_agents_may_carry_them |
| JOB-06 | only attested, live releases take jobs | only_attested_live_releases_take_jobs; nests only_attested_releases_are_admitted |
| AI-01 | the AI review uses the self-hosted model only and is advisory | nests analysis tests; marketplace_m1_tests.rs analysis_stages_are_honest_and_a_failure_blocks_the_review |
| ISO-01 (shared listing) | one listing, one worker per buyer context, no shared mount, label, container or broker token | nests tests/marketplace_m1.rs one_listing_serves_each_buyer_from_its_own_worker; a_job_runs_end_to_end_on_the_shared_listing |
| live (ignored) | real rootless build and the behaviour test under gVisor on a Nests host | nests tests/marketplace_m1.rs live_build_of_the_reference_release, live_behaviour_test_on_gvisor |

### Deviations from the build plan

1. **Build plane:** no separate KVM build VM exists. The build runs on nests-1 as a separate rootless podman build under gVisor (runsc) or crun, with a network only for the dependency fetch and none for the build.
2. **Analysis plane:** also on nests-1, as a command run by an operator, not a separate VM. The behaviour test runs in-process in CI and under gVisor only in the ignored live test.
3. **No signing keys:** OpenBao is skipped, so the attestation is a canonical JSON statement with a SHA-256 digest, written by a staff decision and checked again by admission and by nestd. It is not a cryptographic signature.
4. **Reviewer:** the owner is the only reviewer. Staff may review a first-party (platform) release; the decision and attestation record first_party = true. Every third-party publisher keeps the M0 conflict rule. Having built a release still conflicts, first party or not.
5. **No `nest_job` verb:** the verb table in `verbs.rs` is unchanged (a test now pins that). A job's authority is admission and its payment authorization at POST /v1/nest-jobs, which an agent reaches with its own capability token. A nest_job envelope fails closed like any unknown verb.
6. **Payments:** zero-priced quotes and authorizations only. There is no wallet and no mandate; mandate_id is refused. A retry of an authorization with the same Idempotency-Key rotates the reference instead of storing it.
7. **Events:** job and security events are rows (nest_job_events, release_security_events) and audit entries. They are not yet published on /v1/events.
8. **Results:** the operation's output is inline in a new `result` field of NestJob; result attachments stay empty in M1.
9. **Worker volumes:** plain directories under the marketplace pool's volume_root, not LUKS volumes, and the worker's ledger row lives in nestd's `nests` table so the broker meters it; the first-party lifecycle skips these rows.
10. **Schema:** M0's tables are changed in place (platform owner kind, review stages, release build columns, listing address), because M0 is neither merged nor deployed.

### Hardening (W1, 2026-11-10): what replaced deviations 1, 2, 3, 7 and 9

- **Signed attestations (3).** The statement gains `key_id`; the postbox signs
  `"pigeonpost-nest-attestation-sig-v1\n" || statement` with Ed25519 (seed in
  `POSTBOX_MARKETPLACE_ATTESTATION_KEY_FILE`, never in the repository or database) and stores the
  signature in `release_attestation_signatures` (append-only). `key_id` is `ppk_` and the first 16
  hex digits of SHA-256 over the public key. The postbox verifies it at quote and job admission;
  nestd verifies it at admission and at every dispatch against `[sandbox.marketplace]
  attestation_keys` and refuses everything when that list is empty. The job body's `release`
  object carries `signature` and `key_id`. Rotation: `pigeonpost_nests::attest` module docs and
  `deploy/nests/buildd/README.md`.
- **Build and analysis plane (1, 2).** `pigeonpost-nest-buildd` on a separate host (a shared host for
  now): git source intake by repository URL and full commit id on an allowlisted host, vendored
  dependencies, two builds that must agree, the image saved as an OCI archive, the deterministic
  scan and the advisory AI review. The postbox submits and pulls (`marketplace_source_builds`);
  nests-1 pulls images by digest with its own read-only token. `deploy/nests/buildd/`.
- **Worker volumes (9).** On an `lvm-luks` host every buyer worker has its own LUKS2 thin volume
  (`nest-<id>`, key in `key_dir`, wrapped under the KEK), mounted at
  `<volume_root>/<context>/<id>`, locked until the postbox unlocks the host, taken by
  `nests-backup`, closed when the worker stops. A destroyed worker's id is a tombstone
  (`marketplace_worker_tombstones`) replayed on unlock; a new worker for the same pair gets a new
  id.
- **Events (7).** Typed events on `/v1/events`, ids and states only, written in the same
  transaction as the change:

  | type | to | data |
  |---|---|---|
  | `nest.job.admitted`, `nest.job.progress`, `nest.job.result_ready`, `nest.job.reconciling` | the submitting account | `job_id`, `listing_id`, `release_id`, `execution` |
  | `nest.review.stage_changed`, `nest.review.blocked`, `nest.review.decision`, `nest.review.completed` | the publisher's account (its org owner; the platform owner for Pigeonpost's own) | `review_id`, `release_id`, `publisher_id`, `stage`/`decision` |
  | `nest.security.quarantined`, `nest.security.revoked` | the publisher and every account with a job on the release or listing | `release_id`, `listing_id`, `publisher_id` |

  A job the worker host no longer knows (404 on poll) becomes `reconciling` and is handed over again.
- **Abuse controls.** Per-publisher quotas (`marketplace_publisher_quotas`, defaults from
  `POSTBOX_MARKETPLACE_*`): source builds per day (postbox), jobs per minute per buyer (postbox,
  `429 rate_limited`), concurrent workers and monthly broker tokens (sent as `limits` with each job,
  enforced by nestd before a worker starts or a job lands, `quota_exceeded`). The kill switch:
  `POST /v1/staff/marketplace/{listings|publishers}/{id}/kill` with `{action: kill|restore,
  reason}` (staff with step-up, audited) stops every queued and running job, refuses new ones
  (`409 listing_suspended`), and tells nestd `POST /v1/mp/listings/{id}/stop`, which fences broker
  tokens, stops the sandboxes and closes the volumes; an unacknowledged stop is resent every
  dispatcher tick (2 s).

## Validation (2026-11-03)

- `check_openapi.py` with the hash-pinned validator from `.github/scripts/openapi/requirements.txt`:
  valid OpenAPI 3.1.0, 4 paths, JSON mirror in sync.
- Each standalone schema passes the 2020-12 metaschema and matches its component.
- 45 structural checks pass: the examples validate; the job request rejects nine undeclared
  authority or client-id fields, each missing required field and seven malformed values; the
  authorization request rejects six client-supplied authority fields; the review status enforces
  the approved, rejected, blocked and skipped rules, the fixed stage order and count, and refuses an
  undeclared approval flag and a `deploy` hint.

These are document checks, not a security review or implementation evidence.
