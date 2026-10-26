# Native organization rooms

Continue the Phase 4 native room requirement from #106. Organization rooms use the existing MLS
group engine and safety notices; hosted Nests and Products remain planning only.

1. Confirm organization, mailbox and room response contracts. Discover membership from the
   server, never from a handle or directory entry. Bind every operation to subject, authorization,
   acting mailbox and organization; retire stale responses and clear access-denied data.
2. Add typed room discovery and explicit join/create operations through the existing group
   contract. A directory listing cannot imply active membership, an encryption-ready device or
   permission to create. Preserve provisioning/keying states and fresh server decisions.
3. Add a shared room directory and suitable phone/iPad/Mac entry points. Respect server flags,
   active calls and other sheets. Switch mailbox only after an explicit choice; reuse the normal
   encrypted conversation after joining and activation.
4. Qualify model/wire boundaries, authorization/mailbox races, membership transitions and UI
   behavior, then both builds and hosted CI. Keep native publication batched and Apple review held.

Wire details confirmed from the server: join policy is `public` or `private`; posting is `everyone`
or `admins`. The inbox create route takes mandatory `org_id` and an idempotency key, creates a
keying owner, and returns the organization in active/keying group metadata. Public-room resolution
has no organization field, so bind it to the scoped directory entry and confirm group organization
and explicit membership before navigation. Missing membership never means active in this directory.
Organization/room operations use a fresh scoped token proxy; a stale 401 cannot renew another login.

Progress: [work ledger](native-org-rooms-2026-10-25-progress.md).
