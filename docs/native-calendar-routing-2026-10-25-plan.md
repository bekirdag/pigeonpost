# Native calendar notification routing

Continue the Apple calendar stack after PR100; native releases remain held for the batch and owner's Claude review.

1. Parse typed calendar invitation/reminder payloads with bounded IDs and an explicit recipient mailbox. Classify old `cal:` notices as calendar notices without inferring event IDs from opaque occurrences. Malformed calendar payloads never fall through to the organizer's conversation.
2. On a tap, wait for account identities and capabilities. Select only a mailbox in the signed-in account, reset its inbox scope, configure the calendar synchronously and open the authorized event/occurrence. A locator conveys no permission; the existing event API remains authoritative.
3. Keep pending navigation separate from active detail polling. Queue a tapped event while a calendar editor/detail is open, then open it after that presentation closes. Never discard an editor automatically, RSVP, join or create a contact on a tap.

   For a different inbox's notice while Calendar is open, or while another screen owns the root sheet, ask the user to close that screen and tap again. Do not replace a potentially unsaved screen or change its mailbox automatically.
   Likewise, do not change the calling mailbox during an active or preparing call.
4. Foreground calendar notices use calendar banners rather than message announcements. Do not clear/set the ordinary message badge from this path.
5. Add parser/ownership/legacy tests and actual iPhone/iPad fixture notification navigation. Build both apps and run relevant store/model/UI regressions. Integrate the independent server payload change only after its own CI.

Meeting/guest URL entry remains a separate routing step after this notification contract; no claim of complete link handling yet. No new infrastructure or dependencies.
