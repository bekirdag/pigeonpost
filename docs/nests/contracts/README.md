# Nests marketplace core contracts (proposed)

**Status:** design contracts for the Nests marketplace build plan. Operations marked
`x-pigeonpost-status: implemented` are served by the postbox (`crates/pigeonpost-postbox/src/marketplace.rs`)
behind the per-account `marketplace` flag, off by default, and none of them is deployed. The other
operations are proposed only. See "Marketplace M0" below.

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
(`marketplace_tests::the_routes_and_the_contract_match`) fails when a route in `marketplace.rs` and
an operation marked implemented stop matching, or when an operation names a policy action its
handlers do not use.

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
