# Progress

- Current main has simple publisher applications, staff certification, app review and public listings. Existing application collects only display_name/org_id.
- Created isolated worktree. Original checkout is an older iOS branch and was preserved.
- Docdex MCP is bound to the original root; initialized the specific new worktree through HTTP. Its index is still building; scoped symbol/impact checks requested through HTTP, with direct source inspection while waiting.
- ShuftiPro official requests/responses docs verified: hosted flow uses Basic auth, request.pending verification_url, and signed raw response callbacks. New-account signature uses SHA256(raw body + SHA256(secret)); older accounts require explicit legacy configuration.
- Plan recorded before implementation. ShuftiPro account/environment clarification pending; synthetic tests can proceed independently.

## Implementation and initial validation
- Added publisher_onboarding.rs: additive encrypted dossiers/documents, versioned drafts, typed fields, bounded evidence uploads, review queue, exact-revision staff decisions, requested document categories, and public-only approved profile projection.
- Existing certification URL now routes through the dossier review guard; old clients cannot approve display-name-only applications. Existing certified publishers are preserved.
- Staff queues support state, country, applicant type, text search, sort and pagination; submission emits a durable event to reviewers. Applicant decisions emit existing publisher account events.
- Added six-step wizard, staff dossier/evidence UI and partners.html with publisher/app pages. Mobile preview at 390px has no horizontal overflow; no live applicant or identity data used.
- Required Docdex delegation used configured load-balanced Qwen. Worktree index eventually became ready; HTTP symbol and impact calls succeeded after initial timeouts. Graph confirms main.rs and marketplace tests consume the application modules; JS classic-script links checked directly.
- cargo check -p pigeonpost-postbox --tests passed. Docdex run-tests rejected this inline Rust test target (requires integration test target); used focused Cargo test filter. Initial four onboarding tests passed; subsequent review response changed to 201, exposing one stale assertion, which was corrected. Existing marketplace regression found missing-record handling returned conflict instead of 404; fixed the guard to preserve old semantics.

## Regression fixes and browser checks
- Initial full postbox run: 843 passed, 3 failed, 2 ignored. Fixed the existing org webhook fixture to submit a complete dossier, disambiguated new helper names for the route-policy scanner, and classified all four onboarding tables in purge policy.
- Added consented erasure of personal publisher evidence and a provider-retention evidence gate; the real-schema erasure test checks that a second publisher keeps its records.
- Added KYC reference/digest replay tests, country/IBAN checks, pending hosted-link resume, additional-document revision checks, and a 3 MiB evidence upload to exercise the route body limit.
- Preserved legacy publisher certification/revocation display. Added read-only applicant details and staff bank-country display.
- Browser validation in the existing shared Chrome: company and individual wizard steps, Turkish evidence labels, staff request-more-information action, public partner directory and publisher/app links. All at 390px without horizontal overflow. Console errors were unrelated MetaMask extension connection errors; no application JavaScript errors observed.
- Added docs/developer-onboarding.md and commented ShuftiPro environment settings. No real credentials, applicant records, provider transactions or production deployments used.

## Final validation
- Full postbox run completed: 846 passed, 1 route-policy false positive, 2 existing ignored tests (849 total). The scanner resolves callees by unqualified names and confused the callback's header/database calls with unrelated policy-bearing functions. Isolated callback persistence and signature-header access; retained the explicit signed-callback exemption. Focused route-policy rerun: 2 passed, no failures. All 847 active tests have therefore passed across the full run and focused correction.
- Company workflow includes successful 3 MiB uploads, submission locking, request for new bank evidence, resubmission, approval, private access and public projection. Individual verification rejects incomplete KYC; callback signatures and identity/reference replay binding pass.
- JavaScript syntax checks, Python erasure-script compilation and git diff --check passed. Real-schema erasure tests confirmed provider-evidence gating and preservation of another publisher's records.
- Implementation is local on codex/developer-onboarding-20261114. Live ShuftiPro account configuration and an environment-specific provider smoke test remain deployment prerequisites; no production deploy performed.
- Final onboarding rerun after callback refactor: all 5 tests passed (including 3 MiB uploads).
