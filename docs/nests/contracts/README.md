# Nests marketplace core contracts (proposed)

**Status:** design contracts for the Nests marketplace build plan. Nothing here is implemented or
deployed. The routes would sit on the postbox member API (`/v1`) behind a `nests_marketplace`
feature flag that does not exist yet.

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

The CI `openapi` job checks only the two production specs today. Add this file to its argument list
when implementation starts.

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
