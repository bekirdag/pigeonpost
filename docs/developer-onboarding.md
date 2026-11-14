# Developer applications and partners

The account publisher section opens a six-step saved application: applicant, legal details,
verification, bank details, public profile, review and submission. Private details stay in the
server vault; the browser does not save them in local storage. Company applications need legal
name, registration/tax identifiers, address/contact, DUNS or an explanation, registration and tax
evidence, bank details and consent. Country labels include Türkiye (faaliyet belgesi and vergi
levhası), US and UK; other countries use official registration/activity and tax equivalents.
This is an operational evidence checklist. Reviewers must assess country-specific equivalence
and can request additional documents; it is not an automated legal-compliance determination.

Individuals use hosted ShuftiPro document and face verification. A browser redirect does not
approve KYC. A signed accepted callback with both checks passing must match the current
identity and session. Changing name, applicant type or country invalidates previous verification.
No identity images or biometric evidence are stored by this integration.

## Review and publication

Staff → Developer applications provides status/type/country filters, publisher/ID search,
oldest/recent/name ordering and 25-item pages. Open an application to inspect the submitted
fields, privately download evidence, claim review, request information, approve or reject.
Every decision requires a reason and the current revision. Requested document categories require
new uploads after the request before resubmission. A claim records the reviewer; another
eligible reviewer can take over using the latest revision. Approval still requires complete
company evidence or successful individual KYC. App releases keep their separate security review.
Approved publishers can be revoked using the existing revocation action.

Submission creates durable account events for current reviewers; decisions create publisher
account events. This feature does not add outbound contact-address notifications. Applicants can refresh
their application to see requests and history. Drafts and requests are editable; submitted records
are locked until staff request changes. Rejected applicants can use the existing new-application
flow. Existing approved publishers retain certification and minimal public profiles.

`/partners.html` lists approved third-party publishers. `?publisher=<id>` shows their approved
public profile and published public apps. Only public name, summary and website are projected;
legal names, identifiers, bank details, contacts, evidence and KYC information remain private.
App links open the exact listing in the account marketplace.

## ShuftiPro configuration

Set these in the postbox deployment secret/config store:

- `POSTBOX_SHUFTI_CLIENT_ID` and `POSTBOX_SHUFTI_SECRET`: account API credentials.
- `POSTBOX_SHUFTI_CALLBACK_URL`: public HTTPS postbox URL ending `/v1/verification/shuftipro`.
- `POSTBOX_SHUFTI_REDIRECT_URL`: HTTPS account publisher page, e.g. `/account#publish`.
- `POSTBOX_SHUFTI_SIGNATURE_MODE=legacy` only for accounts using the pre-15-March-2023 raw-secret signature. The default uses the hashed-secret signature.

Missing configuration returns `kyc_not_configured` and preserves the draft. Enable document and
face verification in the chosen ShuftiPro account and verify the account's test/live settings
before live use. The server posts to `https://api.shuftipro.com/`, authenticates with Basic auth,
validates the signature over exact response bytes and accepts hosted links only on shuftipro.com.
Pending hosted links can be resumed for one hour. New starts are limited to one per publisher
per hour, including uncertain provider failures. No live provider transaction was part of local
validation. Complete a provider test flow and signed callback in the deployment environment.

Provider references: [Requests](https://developers.shuftipro.com/docs/verification_endpoints/requests)
and [Responses/signatures](https://developers.shuftipro.com/docs/verification_endpoints/responses).

## Storage, access and operation

Four additive SQLite tables hold dossiers, supporting documents, history and KYC session state.
The existing persistent vault master key must be backed up alongside the database. Dossier
fields, file bytes, review notes and hosted KYC URLs are sealed with purpose-bound encryption.
Workflow metadata, filenames and categories remain queryable. The identity digest binds KYC
sessions to the legal name/country/type; it is not public. Evidence accepts PDF/PNG/JPEG magic
bytes, at most 5 MiB per file and 12 per application; the HTTP body limit permits base64 encoding.
Download as attachments, never render untrusted documents in the staff origin.

Private endpoints require the account owner or active organization owner/admin; delegated app
publisher roles cannot access legal files. Staff reads and decisions require a recent strong
sign-in, reviewer role and no publisher conflict. Private responses use Cache-Control: no-store.
Use the existing marketplace feature switch and deploy matching site/postbox versions together.
The old staff certification URL now requires a complete dossier and exact revision too.

The empty-account purge is blocked by publisher ownership. Consented account erasure removes
the account-owned publisher's dossiers, documents, KYC sessions and private history before
anonymizing the publisher. It preserves other publishers and organization records. For a
publisher with any KYC session, operators must review provider deletion/retention first and
record `shuftipro_retention_reviewed: true` in fulfillment evidence. Local erasure cannot delete
vendor-held identity evidence; record that external work and backup expiry in the deletion case.
Bank collection does not create payouts, verify bank ownership or expand payout eligibility.

## Validation

Run `cargo test -p pigeonpost-postbox --bin pigeonpost-postbox` for onboarding, marketplace,
authorization routing and real-schema erasure regression tests. `tests/publisher-onboarding/browser.html`
is a synthetic no-provider UI fixture for saved steps, country evidence and staff review.

## Static deployment

Install `deploy/sites/partners-location.conf` in the Pigeonpost origin snippets directory and
include it inside only the `pigeonpost.dev` server. Validate and reload the existing webapps nginx
master. It enables same-origin scripts and public postbox reads for `/partners` and
`/partners.html`, preserving the script-free default for other public pages. Install JavaScript
and CSS before the updated account/staff/partners HTML; these references carry an onboarding
cache version. Back up affected files outside the web root and verify served hashes afterward.
