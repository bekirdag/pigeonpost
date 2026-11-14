# Developer onboarding and partner directory

## Outcome
A resumable application replaces one-click publisher certification. Applicants choose company or individual, provide legal/contact/address details, country-specific evidence, bank/payout details, DUNS or a reason unavailable, and public publisher information. Individuals complete hosted ShuftiPro document and face verification. Staff review a private searchable/filterable queue, request additional documents with a reason, and approve/reject exact revisions. Public partner pages expose only approved public profile fields and published public apps.

## Implementation order
1. Add an additive dossier/document/history schema alongside publisher_profiles. Keep existing publisher IDs and app-review workflow. Draft/in_review/needs_information live in the dossier; certification remains enforced in publisher_profiles. Existing approved publishers remain unchanged; unreviewed legacy applications must complete a dossier.
2. Add encrypted private dossier and bounded document APIs, revision checks, submission completeness checks, review transitions and audit history. Country requirements are an extensible operational checklist, with Türkiye's faaliyet belgesi and vergi levhası and equivalent registration/tax evidence elsewhere, not a claim of exhaustive local law.
3. Integrate ShuftiPro hosted verification. Credentials stay server-side; validate signed raw responses/webhooks and bind references to applicant revision. Redirects never approve identity. Store status/reference only, not vendor ID images or biometric data.
4. Replace publisher apply UI with a numbered, saved wizard; show staff requests and allow resubmission. Extend staff queue with status, type, country, search, sorting, pagination, dossier/document viewing and reasoned decisions.
5. Add public partner list, publisher page and approved public apps; link from marketplace/publish pages. Never return legal dossiers, IDs, bank details, contacts, documents or KYC evidence publicly.
6. Test authorization, conflicts, incomplete submissions, revision races, re-review, encrypted storage, signed callbacks/replay, public privacy and UI navigation. Validate builds and browser rendering. Document configuration and deployment requirements.

## Constraints
- Reuse existing role/step-up/conflict protections, vault and app security review. No new dependencies unless necessary.
- Company verification and individual identity verification do not approve app binaries; existing release review remains mandatory.
- Bank collection does not create payouts or change payout-country eligibility.
- No live KYC transactions, production deployment or actual applicant data needed for development.
- Use isolated worktree codex/developer-onboarding-20261114 based on origin/main 3ccf6660.
