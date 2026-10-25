# Native machine confirmation progress

- Isolated worktree/branch created from Apple integration `e445231`; its full CI candidate stays
  frozen. Existing pairing foundation has 54 model and 47 transport checks; actual Session
  lifetime/controller qualification has 20 checks plus passing shared runner and app builds.
- Verified the normative OpenID Connect authorization-code rules against the primary specification:
  direct fixed-token-endpoint TLS validation is permitted, and issuer/audience/expiry/nonce,
  requested authentication context and freshness still require validation. No arbitrary ID-token
  input or authority URL may enter this flow.
- The shared confirmation store checks subject, authorization, operation and proof before and
  after every suspension, including token reads and renewals. Denial re-resolves the final
  state; ambiguous decisions offer a status check. Code edits retire the old proof while
  preserving Retry-After. Strong sign-in resolves another preview and never approves by itself.
- The shared Apple screen shows the organization, agent inbox and explicit creation, machine
  label, requested access, network hint, terminal number and expiry. Approval requires a
  1.5-second hold and confirmation; accessible actions also confirm. The scanner, trusted links,
  dashboard menu and Settings feed the screen after existing sheets close. Server capabilities
  and active calls gate entry; codes and previews stay in memory.
- Qualification so far: actual Session **52**, proof **31**, pairing **54**, actual transport
  **47**, confirmation store **19** checks. Shared runner and both app builds pass. All **8 UI
  cases pass on each local iPhone/iPad**, including short press, a new hold after verification,
  code edits, expiry, delegation denial, denial races, Settings dismissal and feature-off entry.
  A native Button needed a simultaneous long-press gesture; replacing it with a custom HStack
  broke disabled accessibility semantics and was rejected. Final proof-dialog retirement is
  passes a focused follow-up on both devices.
- The first Mac inbox stress run failed on picker cycle 7 during concurrent simulator UI tests.
  The serial run passes **197 checks**, with the failed run retained as evidence. Hosted CI follows.
- Live realm audit confirms native S256, default email/email_verified mappers and the web
  client's dedicated LoA-2 OTP flow with a 600-second maximum age. Native lacked its mapping
  and binding; these two fields now match the web client, with a restricted server-side backup,
  metadata comparison, and passing normal/strong anonymous redirect preflights. Other native
  client metadata and the web client are preserved. A real OTP round trip is still unqualified.
- No real device pairing, org-feature enablement or store submission. Signed association/profile
  checks, genuine authentication and owner device tests remain release evidence; Apple App
  Review stays on hold for Claude.
