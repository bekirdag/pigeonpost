# Native calendar and meeting link entry progress

## Intake

- Reused the clean owned native-round2 worktree on `codex/native-calendar-links-20261025`, based on PR102 `031630c`. No code changes yet.
- Inspected native URL handlers, the scheduled-call bridge, calls-service client contract, Session subject/token methods, PostboxClient retry behavior and the existing `m.js` page.
- `m.js` already reads/removes invitation fragments before requests, then requires explicit RSVP/join. Native member bridge intentionally strips guest tickets/roles; guest links must not be forced through that bridge.
- Current Apple associated-domain file covers only `/g/grp_*`; meeting links currently remain browser URLs. Member preview needs an explicit call occurrence before constructing the existing ScheduledJoin request.
- Docdex web discovery returned unrelated results for the Apple API query. Verified SFSafariViewController against Apple's official API reference and the installed SDK header instead; its initial URL must be HTTP(S), and page interactions stay in the browser interface. Associated-domain fragment rules still need primary-source verification before editing the metadata.

## Status

## Implemented and locally checked

- Uncommitted parser/model/lookup and member/guest link routing now exist in `Shared/Calendar/CalendarLink*.swift`, `CalendarMeeting*.swift` and `CalendarGuestLinkBrowser.swift`, with both app entry points and native sheet hosts connected. Internal SwiftUI links use the same parser; external/custom-scheme handlers are connected. Guest URLs use Safari/system browser without member token injection.
- The lookup uses a scoped token provider that retires token reads/401 renewals when its operation/context changes. The native view binds its preview to Session subject, mailbox and calendar generation, masks old choices immediately on a context change, and requires explicit voice/video plus occurrence selection.
- Pure URL/occurrence suite: 56 checks pass. Actual PostboxClient/URLSession lookup suite: 20 checks pass, including 401 renewal, stale 401 suppression, reordered success, sign-out/cancel and privacy on permission errors. All traffic is synthetic `.invalid` transport.
- iOS Debug and universal Mac builds pass. Four actual meeting preview UI cases pass on each owned iPhone/iPad: single occurrence, overlapping choice, unavailable meeting and capability-off. These checks do not place a real call; the existing scheduled bridge has separate runtime qualification.
- Local test drafts were rejected: one proposed invalid-URL list included a valid guest URL; the UI draft checked hittability rather than disabled/enabled state. Primary tests assert the actual contract.
- Source entitlements previously omitted Associated Domains on both platforms. Added the inbox domain on iOS, Mac App Store and Developer ID variants; all three plists and the shared iOS/Mac AASA identity validate. AASA routes plain/display-form member codes and leaves canonical personal guest links in the browser. Signed-profile capability audits and actual external-link qualification remain open; these are not established by unsigned simulator builds.
- Scanner entry is being integrated through a deferred request delivered after its sheets close. Settings closes its inner scanner before the inbox opens the preview; camera start/stop now share an ordered background queue. Hardware camera behavior remains a real-device check.
- Server notice PR101 is merged/deployed at main6950c29, image `calendar-notice-e5cea04`, with unchanged configuration, zero restarts and public health200/capabilities401. Parent #96/#98/#100/#102 now include the Select All test correction and are in fresh hosted qualification.

## Final local qualification

- Final iOS/scanner build and universal Mac build pass. Full shared runner, including all76 new link/lookup checks, passes. The native Mac inbox harness compiles; its interactive runtime is a hosted check, not a local pass.
- Two additional real UI cases per device pass with synthetic barcode delivery through the actual scanner controller: direct inbox scanner and nested Settings scanner both dismiss before the native meeting preview opens. Thus six focused meeting/scanner cases pass per device across the two runs. Physical camera capture remains an owner device check.
- The Nginx association resource was served as `application/octet-stream`. Prepared an exact-location correction, validated candidate syntax, compared the original config before applying, backed up the original on Raijin at `/var/backups/pigeonpost/aasa-mime-20261025T141501Z`, and reloaded only the dedicated webapps master. Origin/public Apple-agent probes now return200 `application/json`; the existing public AASA bytes and inbox/main-site health are verified. The new member-link AASA bytes are not deployed yet. Reusable snippet: `deploy/nginx/pigeonpost-associations.conf`.
- Private evidence: `native-calendar-links-{iphone,ipad}.xcresult`, `native-calendar-links-scan-{iphone,ipad}.xcresult`, `validation/native-calendar-links-*`, and `aasa-mime-{prepare,apply,public-verification}.json`. No production messages, invites or calls used.

## Still open in this branch

Signed domain capability/profile audit on every distribution variant, genuine OS external-link/browser verification and hosted CI remain release gates. Do not publish a Developer ID build claiming the new entitlement without an authorizing embedded profile and a launch check. Final wording-only changes pass both the iOS Debug and universal Mac builds. No store submission. Legacy machine sign-in retains its existing issuer-only URL guard.
