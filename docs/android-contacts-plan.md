# Android contacts and free names

Implement the reviewed `docs/contacts-and-phone.md` contract without changing the Android build currently in store review.

1. Add typed phone, discovery, SMS-invitation and profile-name APIs over the existing authorized transport. Validate bounded responses and never send device contact names or local aliases.
2. Bind every request, imported snapshot and confirmation to the accepted login and current mailbox. Retire requests on sign-out, account/mailbox replacement or feature withdrawal. Never retry an SMS mutation automatically.
3. Add Settings pages for phone verification/discoverability, finding people, and free display names. Request Contacts permission only after the explicit disclosure; normalize numbers locally with a country override. Preserve partial discovery results and respect the new-account quota. Require a separate confirmation for each invitation.
4. Add account-private local aliases and show labels in conversations while keeping the real address visible. Free avatars and paid handles retain their separate meaning. Clear aliases on sign-out.
5. Test hash vectors, response validation, feature gates, delayed callbacks after identity changes, bounded batching, explicit invitation consent and local-only names. Compile app and instrument permission-denial/confirmation flows where a device is available.
6. Open a reviewed PR and qualify CI. Do not replace the already-submitted 0.4.2 build; collect these changes for the next store batch.

Dependency order: typed contract → authorized store → platform address-book adapter → Compose screens and peer labels → tests/build. Existing `GroupTransport`, `AuthorizationLease` and `OrganizationRoomStore` provide the accepted-login pattern. No new paid services or dependencies.
