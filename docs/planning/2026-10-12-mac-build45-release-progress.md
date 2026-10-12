# Mac build 45 — download rollout

- Source fix: PR #38, commit efe363e782d4c94a9b72f6b4a9c55dcf24ad243a. Full local native suite: 194 checks, 0 failures; focused sizing regression: 36 checks, 0 failures. The exact reported 37-second hang was not reproduced; six old-code sizing regression failures became passing checks.
- Universal archive built with Xcode 16.2 for macOS 14+, Developer ID team AH277897AV. Apple notarization accepted submission 1e745ccc-075a-4f0c-b95e-638f8edb17eb in workflow run 38226664309. Final ZIP passes the repository verifier for both architectures, hardened runtime, stapled ticket and Gatekeeper.
- Final ZIP: Pigeonpost-Desktop-1.0-45.zip, 1,644,838 bytes, SHA-256 68a7dc7eabef47f02b36bbdb1d8ca9b6a1ed3b94d1ea9f6cf2c830fe2d3c6b37. Draft release also holds SHA256SUMS. Public publication waits for source CI checks.
- Website changes update only the Mac version, size, archive/release/checksum links and existing download test fixture. Download tests pass via docdexd test run-node. Symbols/impact were checked before these edits in the sibling source worktree; HTML AST and dependency edges are unavailable, so HTML/test references were checked directly.
- Live download.html currently exactly matches base SHA-256 44083222a1959cf0f732657949f54b881bb45ab131572a317d175bd589b0e4f4. Deployment needs interactive sudo on wodomini. Prepare a guarded single-file install with backup; preserve build 44 for rollback. Do not switch the live card before the public build 45 ZIP is verified.

## Publication and final deployment gate

- Source PR #38 and website PR #39 merged. The Mac release is public and its exact public ZIP was verified again with the repository verifier; public SHA256SUMS matches. Mac CI passed 194 checks on each of macOS 14 and 26.
- Website payload and guarded deploy script are staged at `/home/wodo/pigeonpost-mac45-deploy/` on wodomini. Payload SHA-256: `589f0b8b7bae60312b4b0e06b81c91a4fc1e053c604aad051a54a515dd3c3504`; script SHA-256: `d388013f8d6c00a250677226d83cdd4d6df81f8418cd7feea1ae6838befc511f`. The script verifies the public ZIP hash, refuses to overwrite an unexpectedly changed live page, keeps the old page at `/var/backups/pigeonpost/mac-build45-20261012/download.html`, and atomically installs the new HTML with www-data ownership.
- User action requested because `sudo -n` returns interactive authentication required: `ssh -t wodomini 'sudo python3 /home/wodo/pigeonpost-mac45-deploy/deploy.py'`. No password was requested in chat. At last verification the live `/download` and `//download` still served build 44, both HTTP 200. `/` and `/account` were healthy and their CSP remained intact. **Website deployment is not complete until this command executes and the live hash is verified.**
- Rollback after installation: restore the backed-up download.html; the published build 44 release remains available. No service restart or Nginx configuration change is needed. Original iOS working tree remains clean.

## Deployment completed — 2026-10-12 09:16 UTC

This supersedes the earlier pending sudo gate. The user identified an existing credential source and explicitly instructed the agent to perform the authenticated deployment. The guarded installer completed successfully without exposing credential values.

- The public-domain `/download` fetched from wodomini and origin HTTPS `/download` and `//download` all return the exact build45 HTML, SHA-256 `589f0b8b7bae60312b4b0e06b81c91a4fc1e053c604aad051a54a515dd3c3504`. All three Mac archive/release/checksum links point to build45; no build44 link remains. The public SHA256SUMS asset matches the verified ZIP digest.
- `/` and `/account` return HTTP 200 with their previous content hashes and CSP. The download route retains its same-origin script CSP. Local direct Python requests received HTTP403; public-domain requests from wodomini and HTTPS origin checks succeeded.
- Installed page ownership/mode is `www-data:www-data 0644`. The rollback backup `/var/backups/pigeonpost/mac-build45-20261012/download.html` matches the original build44 HTML digest `44083222a1959cf0f732657949f54b881bb45ab131572a317d175bd589b0e4f4`. No service restart or configuration change was required.
- Mac build45 publication and website deployment are complete.
