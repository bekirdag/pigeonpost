# Apple phone contacts and free names

Implement the reviewed phone/contact contract on iPhone, iPad and Mac using their shared Swift model/UI. Preserve the Apple build already in review and keep this work separate from first-message PR #180.

1. Add bounded Codable wire models and API methods; bind all requests, imported contacts and SMS confirmations to the current authorization and acting-inbox generation.
2. Add native Contacts permission only after a clear disclosure. Respect limited iOS access and an explicit choice of additional contacts. Normalize with the planned free MIT PhoneNumberKit dependency, hash locally, upload no names, preserve partial results and budgets.
3. Add phone verification/unlink/discoverability, found contacts and one SMS-invite confirmation per recipient. Use the system one-time-code hint; never read a mailbox or SMS database.
4. Add a skippable free-name prompt, profile visibility and private local aliases; keep addresses/paid handles distinct and free avatars available. Share behavior across iOS/iPadOS/macOS.
5. Extend core, scoped API and native UI fixtures for permission denial/limited access, actor replacement, quotas, payload privacy, name precedence and explicit invitations. Build all three targets and inspect the new screens.
6. Update privacy/release documentation, qualify CI and review before merge. Gather changes for the next store batch; no replacement of the current submission.

Dependency order: wire models/API → scoped store → Contacts/PhoneNumberKit adapter → shared screens + platform entry points → fixtures/CI. Use existing Account.scopedClient(acting: true), actingGeneration and AuthorizationLease patterns.
