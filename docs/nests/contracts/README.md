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
(`marketplace_tests::the_routes_and_the_contract_match`) fails when a route in `marketplace.rs`,
`marketplace_m1.rs` or `marketplace_m2.rs` and an operation marked implemented stop matching, or when an operation names a
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

## Marketplace M2: the publisher beta (server and CLI)

M2 opens the review pipeline to third-party publishers and adds listings, a catalog and installations. Everything is behind the per-account `marketplace` flag, off by default; nothing is deployed. Installations are free: every price is zero and `funding` is `test_credits`. The web publisher console and staff dashboard are a later task built on these routes.

### Routes and policies

| Route | Policy action | Who |
| :--- | :--- | :--- |
| GET /v1/publishers | PublisherManage | a person: the publishers they act for, with their role |
| GET /v1/publishers/{publisher} | PublisherManage | the publisher's team; others get 404 |
| GET, POST /v1/publishers/{publisher}/roles; DELETE …/roles/{role} | PublisherManage | read: the team; change: an org publisher's org owner or full admin |
| GET, POST /v1/publishers/{publisher}/templates | PublisherManage | read: the team; create: a certified publisher's admins |
| GET, POST /v1/publishers/{publisher}/builds; GET …/builds/{build} | PublisherManage | same |
| GET /v1/publishers/{publisher}/reviews, …/reviews/{review}, …/reviews/{review}/findings | PublisherManage | the team; findings redacted |
| POST /v1/publishers/{publisher}/reviews/{review}/withdraw | PublisherManage | the publisher's admins |
| GET, POST /v1/publishers/{publisher}/listings; PATCH …/listings/{listing}; POST …/withdraw | PublisherManage | read: the team; change: a certified publisher's admins |
| POST /v1/publishers/{publisher}/listings/{listing}/publish | NestPublish (HumanStepUp) | a certified publisher's admin with step-up |
| GET /v1/marketplace/listings | NestDiscover | anyone; published public listings only |
| GET, POST /v1/nest-installations | NestInstall | a person, for their own account |
| GET, DELETE /v1/nest-installations/{installation} | NestManage | the installing account, a person; others get 404 |
| PUT /v1/nest-installations/{installation}/configuration | NestConfigure | same |
| POST /v1/nest-installations/{installation}/update, …/rollback | NestManage | same |
| GET /v1/staff/publisher-applications, GET /v1/staff/reviews | NestReview | a reviewer with step-up |
| POST /v1/staff/publisher-applications/{application}/decisions | NestReview | a reviewer with step-up and no stake |
| POST /v1/staff/publishers/{publisher}/revocation | NestQuarantine | same |
| POST /v1/staff/listings/{listing}/moderation | NestQuarantine | same |
| GET /internal/v1/marketplace/build-requests; POST …/build-requests/{build}/failure | ops service token | the build plane |
| POST /internal/v1/marketplace/builds with `build_id` (M1 route) | ops service token | the build plane, answering a publisher's build request |

The team of a publisher: a personal publisher's own account; for an org publisher, the org's owner and full admins and active members holding a live `publisher_admin` (acts, publishes with step-up) or `publisher_finance` (reads only) role. `GET /v1/publishers/{publisher}/reviews/{review}` is the contract's `getNestReview`, now implemented.

### How a third-party release gets to a buyer

1. A person applies (M0). A reviewer with step-up and no stake certifies or rejects it; the decision is in `publisher_certifications` and on `/v1/events` as `nest.publisher.decision`.
2. The publisher creates a template (its package name) and asks for a build of a pinned commit from an https repository. The build plane lists queued requests, builds them as in M1 and posts the build record with `build_id`. The manifest must be the request's name and version, pass the shared package rules and stay off `/pigeonpost/` addresses. The release is authored by the person who asked, so the conflict rule sees them.
3. The analysis stages are M1's routes, now in order with no skipping (intake, deterministic_scan, ai_analysis, behavioral_test; `409 stage_out_of_order`), once each (`409 stage_already_recorded`), and idempotent: the same record again returns `200` with `replayed: true` and no new event. Each stage bumps the review's `revision` and sends `nest.review.stage_changed`; a failure also sends `nest.review.blocked`. The staff decision sends `nest.review.decision`, and `nest.review.completed` for an approval or rejection.
4. The publisher reads the contract's `NestReviewStatus` and redacted findings: their own scan findings in full, behaviour-test findings by rule only, the AI review as a count.
5. An approval publishes nothing for a third party. The publisher creates a listing (draft; the address is the release's `listing_address`) and publishes it with step-up on an attested, live release. Visibility is public (in the catalog), unlisted (by id) or private (the team only).
6. A person installs the listing for their account. The installation pins the release; its configuration is validated against the release's `config_schema` and its secrets are sealed. Jobs on a third-party listing need an installation (`409 installation_required`) and run the installed release; the worker receives the stored configuration with the schema's defaults, never a secret.
7. Update moves to a newer attested release (the listing's current one or a named one), rollback goes back once to the release before the last update, uninstall deletes the configuration and secrets, cancels open jobs and asks nestd (`POST /v1/mp/workers/remove`) to delete the buyer's worker and volume.
8. Staff can suspend or reinstate a listing and revoke a publisher (its listings are suspended, queued builds cancelled, open reviews undecidable). Release quarantine and revocation stay M1's.

### Configuration schema

`config_schema` in `nest-release.toml` is a subset of JSON Schema 2020-12, implemented once in `pigeonpost_core::nest_package` and run by the postbox (build intake, install, configure, update, rollback), the build plane (`pigeonpost_nests::release::Manifest::parse`) and `pigeonpost nest validate`. One flat object with `additionalProperties: false`, at most 32 `string`, `integer`, `number` and `boolean` properties; strings need `maxLength` (at most 4096) or `enum`; `writeOnly: true` marks a secret. Every other keyword is refused by name. `examples/nest-config-fixtures.json` holds the shared fixtures; the core, CLI and postbox tests all assert the same `[path, code]` issues for them.

### Local kit

```text
pigeonpost nest validate path/to/package [--config example-config.json] [--json]
```

Offline: no identity, no home directory, no network. It reads `nest-release.toml` and checks for `Containerfile`, reports errors (malformed, refused at intake) and blocked items (capabilities, syscalls, egress beyond the broker, external writes: what the deterministic scan would block), and exits non-zero on any.

### Data model additions

`publisher_roles`, `publisher_certifications` (append-only), `nest_builds`, `nest_installations`, `nest_installation_secrets` (vault-sealed, bound to their row), `nest_installation_events` (append-only, ids and versions only); new columns `publisher_profiles.decision_reason`, `nest_listings.description`, `template_id`, `moderation_reason`, `published_at`, `review_cases.revision`, `build_id`. nestd: `JobIn.config` and `POST /v1/mp/workers/remove`.

### Exit tests

| ID | What it shows | Where |
| :--- | :--- | :--- |
| SEC-03 | an unattested release is never published or installed; approval publishes nothing for a third party | marketplace_m2_tests.rs listings_publish_only_attested_releases_and_honour_visibility, third_party_releases_go_through_the_review_stages_in_order |
| SEC-04 | revocation and moderation stop use; a revoked publisher creates nothing | revocation_suspends_listings_and_stops_new_work, listings_publish_only_attested_releases_and_honour_visibility |
| SEC-06 | publishers see redacted findings only | third_party_releases_go_through_the_review_stages_in_order |
| SEC-07 | staff conflict rule for third-party publishers and applications | staff_certify_publishers_and_only_staff_without_a_stake, third_party_releases_go_through_the_review_stages_in_order |
| CFG-01 | the configuration subset, server and CLI on the same fixtures | the_server_agrees_with_the_shared_configuration_fixtures; core nest_package tests; CLI nest_cmd tests |
| install lifecycle | pinning, secrets at rest, update, rollback, uninstall, reinstall | installations_pin_configure_update_roll_back_and_uninstall; nests configuration_reaches_the_worker_and_an_uninstall_removes_it |
| roles | org publisher roles and who may publish | org_publisher_roles_decide_who_may_publish |
| flag | every M2 route is hidden while the flag is off | every_m2_route_is_hidden_while_the_flag_is_off |

### Deviations from the build plan

1. **Publisher roles** live in `publisher_roles`, not as new `org_roles` values: adding values to the live `org_roles` CHECK would mean rebuilding that table. Same effect: the org's owner and full admins grant them, only to active members, and they count only while the person stays an active member.
2. **Installations** are `/v1/nest-installations`, not `/v1/nests/{nest}/…`: a marketplace installation is not a Phase B Nest (one agent and its workspace), and those routes already exist. Account installations only; org installations are not built yet.
3. **Visibility:** public, unlisted and private. Invite-only and organization are refused (`visibility_not_available`) until their grants exist.
4. **Secrets** are sealed with the postbox vault (XChaCha20-Poly1305 under a purpose subkey, bound to installation and name). They are not delivered to the worker: the plan keeps customer secrets with brokers, and the tool broker that would use them is not built. OpenBao is still skipped.
5. **Builds:** the build plane polls `GET /internal/v1/marketplace/build-requests`; there is no source-connection (GitHub App/deploy key) record yet, so the repository must be fetchable read-only by the build host.
6. **Events** go on `/v1/events` for the publisher's team (typed events, ids and states only; clients see them with the `account_events` flag). Org webhooks are not wired.
7. **Hosted test Nests** (R-70's staging postbox and test pool) are not built; the local kit is.
8. **Schema:** M0/M1 tables changed in place again, since neither is merged or deployed.

### What a hosted test environment needs

A separate staging postbox (its own domain, Keycloak client or realm, database and vault key; production handles never reused, OD-9) with the `marketplace` flag on; a nestd instance with a marketplace pool for test Nests on a host other than nests-1 production workers (synthetic data only, short-lived, purged on expiry); the build and analysis commands pointed at that postbox with their own ops token; the mswarm self-hosted route for the AI review; a dependency mirror or allowlisting proxy for the fetch step; a `test-nests` route family (`/v1/publishers/{publisher}/test-nests`) with expiry (`nest.test.expiring`) and a reset/delete action on nestd's existing control API.

## Marketplace M2b: grants, org installations, sources, hosted test Nests

M2b closes the server gaps M2 left: invite-only and organization visibility, org-owned installations, source connections, hosted test Nests on a staging postbox, org webhooks for marketplace events, and the console gaps found by the web UI. Everything stays behind the `marketplace` flag and zero-priced. It replaces M2 deviations 2 (org installations), 3 (visibility), 5 (source connections), 6 (org webhooks) and 7 (hosted test Nests). The staging postbox is set up from `deploy/staging/`.

### Invocation through an org installation

A member sends `x-pigeonpost-installation: <installation id>` with `POST /v1/nest-jobs/quote` and `POST /v1/nest-jobs`. It counts only for the org's Nest admins and members with a live `operate_nest` delegation on the installation's agent address; anyone else gets `404`. Its jobs run in one worker per listing and org (`buyer_context` of `org:<id>`). On an `invite_only` or `organization` listing every quote and job also needs a live grant with `invoke` (`403 grant_required`).

### Events

Org webhooks (`webhooks::EVENTS`) may now subscribe to `nest.publisher.decision`, `nest.build.failed`, `nest.review.stage_changed|blocked|decision|completed`, `nest.listing.updated`, `nest.listing.grant_changed`, `nest.installation.changed`, `nest.test.changed`, `nest.test.expiring` and `nest.test.expired`: an org publisher's events (with `publisher_id`) and the org's own installations. Payloads are ids and states only. An invitee hears `nest.invite.received` and `nest.invite.revoked` on `/v1/events`.

### Routes and policies

| Route | Policy action | Who |
|---|---|---|
| GET, POST /v1/publishers/{publisher}/listings/{listing}/grants | PublisherManage | read: the team; create: a certified publisher's admins |
| DELETE /v1/publishers/{publisher}/listings/{listing}/grants/{grant} | PublisherManage | the publisher's admins |
| GET /v1/marketplace/invites; POST /v1/marketplace/invites/{grant}/accept, .../decline | NestInstall | a person holding the invited address |
| GET, POST /v1/orgs/{org}/nest-installations | NestInstall (org View / ManageAgents) | read: active members (admins all, operators theirs); install: a Nest admin |
| PUT, DELETE /v1/orgs/{org}/nest-installations/{installation}/operators/{member} | NestInstall (org ManageAgents) | a Nest admin |
| GET, POST /v1/publishers/{publisher}/sources; GET, DELETE .../sources/{source} | PublisherManage | read: the team; change: a certified publisher's admins |
| GET /internal/v1/marketplace/build-requests/{build}/source-credential | ops service token | the build plane, queued builds only |
| GET, POST /v1/publishers/{publisher}/test-nests; GET, DELETE .../test-nests/{test_nest}; POST .../reset; GET, POST .../jobs; GET .../jobs/{job}; GET .../logs | PublisherManage | staging postbox only; read: the team; change: certified admins |
| GET /v1/publishers/{publisher}/listing-stats | PublisherManage | the team; counts only |
| POST /v1/marketplace/listings/{listing}/reports | NestInstall | a person who can see the listing, 10 a day |
| GET /v1/staff/listing-reports; POST /v1/staff/listing-reports/{report}/close; GET /v1/staff/listings | NestReview | a reviewer with step-up |

### Data model additions

New tables include nest_listing_grants, publisher_sources (private key sealed with the vault, wiped on delete), nest_test_envs, nest_test_jobs, nest_test_events, and nest_listing_reports. New columns are nest_installations.owner_kind (account or org), nest_installations.agent_address (org installations), nest_jobs.installation_id (jobs run the installation's pinned release and configuration), nest_builds.source_id, and nest_listings.price_currency (TRY, USD, EUR) and price_term (one_off, monthly). The nestd config adds [sandbox.marketplace] test_pool flag and verify_test_admission.

### Exit tests

| ID | What it shows | Where |
|---|---|---|
| R-52 | invite and organization grants decide discover, install and invoke separately; revoke stops invocation | invite_grants_decide_discovery_install_and_invocation_separately, organization_grants_follow_live_membership |
| org install | Nest admins manage, operators read and invoke with x-pigeonpost-installation, offboarding and delegation removal end use, uninstall retires the agent | org_installations_follow_admins_and_operators_and_end_with_offboarding |
| R-62 | host allowlist, deploy keys sealed and never returned, credential only for queued builds, delete wipes the key | source_connections_seal_deploy_keys_and_feed_builds |
| R-70 | staging guard, payment refusal, environment header | test_nests_and_the_staging_marks_exist_only_on_a_staging_postbox |
| R-70 | intake-passed releases only, test pool only, test statements, reset, logs, expiry warning and end | test_nests_run_intake_passed_releases_in_the_test_pool_until_they_expire; nestd dispatch test_pools_and_production_pools_admit_disjoint_statements |
| webhooks | org publisher and org installation events reach org webhooks, ids only | org_webhooks_hear_publisher_and_installation_events_by_id_only |
| console | install counts, reports, staff listing list, price terms, roles by member_id | install_counts_reports_staff_listings_price_terms_and_roles_by_member |

### Deviations and limits

1. Org installations use one of the org's agent addresses (10 x seats), like an org Nest, so operators reuse the operate_nest delegation and its offboarding.
2. Invite-only listings are installed per account; an org installs only organization, public, unlisted or its own publisher's private listings.
3. A test Nest's log is its event log (ids, states, error codes); worker stdout is not exposed.
4. Deploy keys are generated by Pigeonpost (Ed25519); a publisher never uploads a private key. GitHub App connections are not built.
5. Test Nests require the test pool on a staging postbox; a production pool refuses test statements and a test pool refuses production attestations.
6. Prices stay zero; price.currency and price.term are stored for saas_be offers to mirror.
7. Tables changed in place again (not merged or deployed).

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
