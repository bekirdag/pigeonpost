# Approving a machine from the native apps (iOS/iPadOS, macOS, Android)

Server contract: `docs/orgs.md` §8. The web reference is `site-inbox/pair.html` + `pair.js`; the
native flow must behave the same way. Native screens are optional for the Phase 4 beta: until they
ship, the QR opens `https://inbox.pigeonpost.dev/pair.html#CODE` in the browser, which works.

## Entry

- `pigeonpost login --device --handle /acme/agent1` prints a QR of
  `https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH`, the code and a confirmation number.
- The app's scanner (and a universal link / App Link on `/pair.html`, if registered) recognises that
  URL and takes the code from the **fragment**. Accept `^[A-Z0-9]{4}-?[A-Z0-9]{4}$` only; never log
  it or put it in a query string. Keep it in memory only for the duration of the flow.

## Screens

1. `POST /v1/device-pairings/resolve {user_code}` with the signed-in member token.
2. Confirmation screen, one primary action:
   - organization name, the agent handle (prominent), the machine label, requested access
     (`capabilities`), approximate location from `ip_country` labelled "from the network; not proof
     of anything", and an expiry countdown from `expires_at`;
   - the `confirmation` number, large and monospaced, with "Check that your terminal shows the same
     number. If it doesn't, or you didn't start this, choose Deny.";
   - **Hold to approve** (about 1.5 s, with a progress ring; VoiceOver/TalkBack: a custom action
     "Approve" that still asks for confirmation) and a secondary **Deny**;
   - when `can_approve` is false, disable the hold button and show `reason`
     (`delegation_required` → "Ask an agent admin to add you as one of its operators").
3. Approve: `POST /v1/device-pairings/{pairing_id}/approve {user_code}`; Deny: `…/deny`.
4. End states: approved ("You can close this; the machine is signing in"), denied, expired
   (`pairing_expired`), not found (`pairing_not_found`), rate limited (`429`, honour `Retry-After`).

## Step-up

Approving needs a recent strong sign-in. On `401 step_up_required`, read `acr_values` and `max_age`
from the JSON body or the `WWW-Authenticate` challenge, run a fresh Authorization Code + PKCE request
with `acr_values`, `max_age=600`, `state` and `nonce` in `ASWebAuthenticationSession` (Apple) or
Custom Tabs (Android) against the `pigeonpost-prod` realm, store the new tokens, return to the
confirmation screen and require one more hold. Never approve automatically after the sign-in.
`403 step_up_unavailable` means the server has no strong method configured: say so and stop.

## Must not

- Never send the user's tokens to the requesting machine; the machine gets its own credential from
  the server after approval.
- Never treat the label or location as identity proof; only the confirmation number ties the screen
  to the terminal.
