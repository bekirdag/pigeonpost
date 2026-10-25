# Calendar import confirmation — retry correction

Current import confirmation consumes its draft atomically, then returns the created private
events. If that response is lost, an identical retry returns 404. Repeated selection indices can
also create duplicate copies in one request.

1. Reproduce both behaviors through the real HTTP/socket test setup before changing production code.
2. Normalize selection order and duplicates, bound the input and save a sealed confirmation receipt
   in the existing calendar idempotency table under a distinct action. Keep the original response
   and target calendar, bind the receipt to the acting mailbox and canonical request, and preserve
   the existing bounded retention. Do not add a schema, dependency or plaintext content cache.
3. Check current calendar ownership on replay. Changed requests conflict; another mailbox cannot
   recover the receipt. No events, invitations, reminders or account notifications are created again.
4. Test receipt confidentiality, selection changes, recipient isolation, expiry and deleted targets.
   Run focused tests against old/new implementations, then full postbox tests, Clippy and formatting.
5. Publish an independent PR from production main. After qualification, build a pinned image and
   use server-local SQLite backup/quick_check and isolated candidate/rollback startup before rollout.
   Old binaries ignore the new action and retain their previous 404 behavior for consumed drafts.

Apple calendar management stays separate. Native stores remain held for the batch and Claude review.
