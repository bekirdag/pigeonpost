# Native account and encrypted group scope

## Confirmed problem

The session has an authorization generation, but Account identity loading and generic GroupStore lifecycle use only a mailbox or their own delayed reset. A late request can consult new credentials; an old engine can finish saving after sign-out erases its vault. GroupStore also retains engines across mailbox changes and an old refresh can clear a newer refresh task. Room discovery itself is scoped, but its eventual generic group conversation must have the same boundary.

## Implementation order

1. Add a small revocable authorization lease for synchronous local file access. New authorization and local retirement revoke the old lease before changing the grant. A child lease binds the acting inbox. Use parent-to-child lock order; never hold a lock across an asynchronous operation.
2. Snapshot authenticated API operations. Retain only the scoped token proxy, with a weak reference to its source, and check the captured authorization before and after token acquisition/renewal. Account identity loads must reject old completions and allow a new grant to start while an old load is pending. Hide cached identities/capabilities from another person immediately; preserve same-person strong-auth flows.
3. Bind each group activation to person, authorization and acting inbox generation. Retire its engine and pending refresh when the context changes. Gate displayed plaintext and events synchronously; use captured clients and fresh operation ownership across every suspension. Bind vault writes to the revocable lease so retired work cannot recreate erased state.
4. Extend lifecycle task identities to include authorization and acting-inbox generation. Review sign-out ordering and same-inbox reauthorization, including a late old sign-out after another login.
5. Verify controlled credential/request races, delayed group events and refreshes, queued engine operations and vault save/erase races. Run the shared suite, group engine integration, both Apple builds and the affected phone/tablet/Mac screen checks. Record actual outcomes separately.

Calendar authorization and durable retry work follows this foundation in a separate change. No cryptographic protocol change, production feature enablement or store submission is part of this hardening change. Apple review remains held for the owner's Claude review.
