# Account deletion operations

Owner: Wodo Teknoloji A.Ş. privacy operations, operated by Bekir Dağ. Confirmed
requests are created by the signed-in member, persisted in the postbox SQLite
database, and shown to the member with a receipt and a deadline 30 days later.
The public flow requests erasure; it does not claim immediate deletion.

The daily `pigeonpost-deletion-queue.timer` runs the read-only queue reporter and
notifies the operator mailbox if any requests remain. Check timer failures and
mail delivery with normal production service monitoring. Start fulfillment when
a request arrives; escalate any request still pending after seven days. Never
wait until its deadline to start. Verify the queue after every deployment.

## Fulfill a request

1. Read the pending queue on the postbox host:
   `python3 /opt/pigeonpost-privacy/account-deletions.py --db /opt/pigeonpost-postbox/data/postbox.db`.
   Retrieve the matching request, account, subject and verified contact only in
   the private operator session. Check the subject against the identity provider
   and the account row. Do not ask the member to submit the request again.
2. Record a restricted fulfillment note keyed by request ID, without passwords,
   JWTs or message bodies. Obtain a completion-notice destination from the verified
   request contact or the identity provider before removing the sign-in account.
   If none exists, arrange confirmation through the account's mailbox before
   erasure. Explain any legally required retention and its end date.
3. In the `pigeonpost-prod` identity realm, revoke all online/offline sessions,
   remove the user and its credentials/provider links. For Sign in with Apple,
   revoke Apple's user tokens using Apple's revoke API when a token is held; if
   the integration did not retain a token, follow Apple's documented account
   deletion/revocation procedure and record the outcome. Verify the deleted user
   cannot sign in or refresh tokens. The postbox retains a subject hash to reject
   still-valid old access tokens after erasure.
4. Review store and web billing. Account deletion must not depend on canceling a
   subscription. Remind the member how to stop Apple/Google renewal; stop Wodo web
   renewals and detach saved payment methods through MASAAS. Remove unnecessary
   billing profile data; retain only records required for transactions, refunds,
   abuse prevention or law, with a documented expiry. The local tool preserves
   purchase uniqueness under an erased-account identifier so old receipts cannot
   be reused by another account.
5. Inventory public registry/directory leases for the account. Disable operational
   access and endpoint bindings where supported. Historical signed public records
   cannot be rewritten; record their retention separately. Do not transfer a paid
   namespace before its paid term and release hold end. Remove this person's
   complimentary tester approval if present. Preserve other accounts' own copies
   of delivered messages and shared attachment blobs.
6. Stop the postbox and its reaper for the short database/blob operation. Confirm
   no other process writes the database or blob directory. Take a restricted
   rollback backup and record its erasure date (within 30 days of this request).
   Prepare a private JSON evidence file with `request_id` and the following true
   fields, only after actually completing each check:
   `identity_deleted`, `sessions_revoked`, `apple_revoke_reviewed`,
   `billing_retention_reviewed`, `registry_retention_reviewed`, `services_stopped`,
   `completion_notice_arranged`, `backup_expiry_recorded`. Set
   `retain_abuse_evidence` only when a specific security retention basis is recorded.
7. Run the reviewed local tool with `--erase REQUEST_ID --evidence PRIVATE_JSON
   --db DATABASE --blobs BLOB_DIRECTORY`. It refuses requests that are missing,
   completed, or no longer owned by the confirmed subject. It removes that
   account's keys, mailboxes, message copies, contacts, settings, notification
   devices, provider links and unshared files. It clears the request's contact
   and subject when complete. Inspect the result before restarting services.
8. Restart services; verify health, an unaffected account, and rejection of old
   credentials. Confirm completion to the member through the previously verified
   destination, including any specific retained data and its retention period.
   Record successful notice delivery and purge temporary exports. Expire the
   request's rollback backup on its recorded date and enforce the same erasure
   when restoring any older backup.

Never call the old single-mailbox `delete_identity` method as a substitute for
account erasure: it has a different scope. Never run a broad sender/recipient
delete that erases another account's delivered copies. Never mark a request
completed merely because an email was sent or a queue row was created.

Apple permits manual fulfillment within a communicated timeframe, with completion
confirmation: https://developer.apple.com/support/offering-account-deletion-in-your-app/.
