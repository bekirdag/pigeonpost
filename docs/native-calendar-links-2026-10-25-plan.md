# Native calendar and meeting link entry

Continue from PR102 without changing its qualifying source. Native releases remain batched and Apple submission remains held for the owner's Claude review.

1. Parse only exact trusted HTTPS Pigeonpost meeting/invitation URLs and the app's explicitly supported scheme. Bound locator/fragment lengths; reject userinfo, alternate origins/ports, traversal, duplicate token parameters and unknown authority fields. Never log or persist personal invitation fragments.
2. Plain member meeting codes open a native preview. Fetch the existing calls-service scheduled-status contract through the established JSON client using a token provider bound to the session subject, acting mailbox and calendar generation. Late results and 401 renewals cannot adopt another context. Ask for an occurrence and voice/video before handing an exact request to the existing scheduled-call bridge after sheet dismissal.
3. Personal guest and unsubscribe URLs retain the existing web preview, verification and explicit confirmation flow. On iOS use Apple's SFSafariViewController; on Mac use the system browser. Do not inject member tokens or reuse the native member call page for a guest. Opening a URL never approves an RSVP or joins a call.
4. Integrate incoming links and scanner entry while preserving active calls and existing editors. Signed-out member links remain in memory until sign-in; closing/cancelling or changing account clears stale state. Cold-launch/universal-link handling must work as well as an already open inbox.
5. Update associated-domain metadata only after checking Apple's component/fragment rules. Keep personal guest links in their existing browser flow where appropriate; test the actual association JSON and parser behavior.
6. Qualify untrusted URL cases, occurrence bounds, token/header scope, late/reordered/401 responses, capability-off/sign-out, cancellation and actual iPhone/iPad entry UI. Build iOS/universal Mac and preserve the existing scheduled-call/notification regressions. Use fixtures and loopback/.invalid services only.

No new dependencies or paid infrastructure. Server APIs remain authoritative; locators and public codes grant no role. Ordinary native member calls continue using the existing renderer.
