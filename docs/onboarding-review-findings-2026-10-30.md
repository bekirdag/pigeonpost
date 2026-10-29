# Onboarding continuation: review findings

This is a focused source and regression review of the October 29 handoff and its high-risk dependencies, not an external penetration test or a physical-device acceptance report. Source/worktree states and deployment evidence are recorded in `onboarding-continuation-2026-10-29-progress.md`.

| Finding | Change | Evidence / rollout |
|---|---|---|
| Email-code snapshots allowed duplicate use, renewed guessing budgets, stale codes after resend and changed-address ambiguity | Atomic single-use/attempt budget and email recheck in su_iam #5 | 17 Java +15 local Keycloak/Mailpit checks; reviewed image staged, activation waits on Postmark |
| First-message dialogs could outlive the login/mailbox that opened them | Bind Windows/Apple dialog work to originating authorization and mailbox; preserve draft/retry ownership | Windows #173 merged, 492 core/MLS checks; Apple #180 pending CI |
| Discarding the first pending Apple message removed the active composer; a deleted UIKit cell remained in accessibility | Preserve an empty active conversation without inventing history, clear removed cells | 180 Inbox checks and unchanged first-send/discard UI tests pass |
| Expired phone-discovery quota marks repeatedly spent quota; large/partial imports were misleading | Correct expiry upsert and initial batch size; retain checked/unchecked state | #178 merged/live; 596 postbox +2 avatar +250 web checks |
| Meeting notes did not pause until a withdrawal write returned; stale acceptance could win | Immediate local pause, serialized/versioned writes and stale-write compensation | #183 merged/live, 91 combined calls/notes tests at that revision |
| Exact allowlisted commands could run workspace hooks on the host | Run approved argv inside the existing restricted container | #185 pending CI; 21 shell checks +60 real Docker/SSH loopback checks |
| Notes audio decoded after discard could re-enter capture; failure paths retained mutable PCM | Capture/check generation; finally cleanup and late-decoder-output wiping | #186 pending CI; four failing-before regressions, 95 combined checks and real synthetic ffmpeg pipe test |
| Compact storage rollback documentation could treat conversion failures as completion | Require undecodable count and remaining compact-row checks before old-reader rollback | Documentation corrected in #183; no production storage rollback attempted |

## Boundaries checked without a new defect found

- SSH human/step-up/delegation checks, certificate/key/session binding, CA pinning, broker ciphertext boundary and teardown. The loopback suite covers authorization refusals, revoked sessions, key changes, forwarding and exact argv/path refusal. External container-escape and broker penetration testing remains the separate SDS requirement.
- Org API key authority is intersected with the creator's current membership/role on requests; webhook delivery resolves and pins public targets, rejects private addresses and redirects, and avoids ambient proxies.
- Compact row decoding uses bounded lengths/flags and the compatibility reader; deployment must preserve the post-Phase-9b storage reader.
- Streaming trusts the postbox as membership/key-directory authority, as documented. Notes session keys are checked against a pinned signing key; capture takes the device's own microphone track. These checks do not imply a malicious membership authority is excluded.
- Contact discovery is opt-in after phone verification; imports and local aliases are scoped to the login/inbox/device. SMS requires a separate confirmation and server budgets. Unpaid profile pictures/names remain separate from paid handles.

## Remaining acceptance gates

- Postmark authenticated access, dedicated Pigeonpost transactional SMTP/no tracking, one authorized real delivery check, then enable code sign-in and calendar/company invitations.
- Native CI qualification and the next coordinated store batch; current submissions remain intact. Apple privacy labels must include meeting audio before the next owner-approved review submission.
- Owner checks on physical iPhone lock-screen answering/call orientation and Windows/Linux strong sign-in. No real users were called and no real contacts were invited during this review.
- The prior handoff's unrelated roadmap items (native agent SSH screens, Windows operated-agent inboxes, per-org notes policy, delta sync and external cryptography review) remain distinct work.
