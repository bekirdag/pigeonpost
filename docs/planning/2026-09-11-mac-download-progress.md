# Public Mac download progress

## Inspection
- /download and //download currently return HTTP 404; no download page exists in site/.
- Production Nginx serves clean URLs from /var/www/pigeonpost, with script-src 'none' for the general site. The new download route needs a narrow policy allowing its same-origin script.
- The 1.0 (30) Mac archive is Developer ID signed for AH277897AV, notarized and stapled. Gatekeeper accepts it as Notarized Developer ID. Its executable includes x86_64 and arm64; Info.plist requires macOS 14.0.
- Final archive SHA-256: 551898e7ddf71bbe0afa383a274601251ff6af7e8fa64462fd2840ff15ab7db3. GitHub currently holds it in draft release macos-1.0-30; the CLI's latest release is v0.7.15.
- Browser discovery reports no connected browsers. The user was asked to connect one while non-browser implementation, native validation and deployment continue.

## Implementation and local validation
- Added a static page, native download links and manual Apple silicon/Intel choices. Both deliver the same universal ZIP; device detection changes only the recommendation and keeps every manual choice available, including without JavaScript.
- Added a narrow Nginx location include allowing the download page's same-origin script. The general marketing CSP and account routes stay intact.
- Supplied logo_white.png is copied without modification and selected with picture media sources for dark themes on the homepage, account page and download page. Shared branding CSS keeps the logo canvases in a stable header footprint.
- Docdex does not index HTML/CSS and returned empty impact graphs. Direct source/call-site inspection, JavaScript/Python symbols and a DAG export supplemented it. A delegated local-model picture fragment was validated and used. The Docdex runner did not detect the nested web suite, so its Node test command was run directly.
- Ten focused download tests pass, covering both Mac chip families with Intel browser user agents, desktop/mobile platforms, iPad desktop mode, unknown clients and no JavaScript. A reusable native archive verifier accepts the approved build and rejects a wrong checksum before extraction.
- The full web suite passes: 41 tests, zero failures. No app code changed; the previously notarized 1.0 (30) archive is reused unchanged.
- Both architectures passed Developer ID signature, hardened runtime and debugger-entitlement checks. Stapler and Gatekeeper pass. The arm64 executable and the x86_64 executable under Rosetta each launched and remained running; only the test processes were terminated.
- The user clarified they use Codex TUI. Corrected the desktop-only browser instructions with the official MCP command for Playwright; a live browser connection is still unavailable in this running session.

## Live delivery
- Published https://github.com/bekirdag/pigeonpost/releases/tag/macos-1.0-30 from app source 486d16fde2a6fa694e30bf81a7bd5955502ce420. Its public ZIP and SHA256SUMS are downloadable without authentication. The CLI's latest release remains v0.7.15.
- Deployed all seven public files from implementation commit 340cc03 to wodomini. Backup: /srv/backups/pigeonpost-download-20260911-340cc03, containing the prior static site and Nginx configuration. nginx -t passed before and after the narrow include was installed, then Nginx was reloaded.
- /download, //download, /download/ and /download.html return HTTP 200 with the exact source HTML, the intended same-origin script CSP, and no-store caching. Download JavaScript/CSS, branding CSS, white logo, homepage and account HTML also match source hashes. The homepage retains script-src 'none'.
- Downloaded the public ZIP afresh with no authentication. Its checksum matches both the approved notarized archive and public SHA256SUMS. The native verifier passes again on that downloaded copy for both architectures, signing, hardened runtime, stapled ticket and Gatekeeper.
- PR https://github.com/bekirdag/pigeonpost/pull/5 merged as 196c38f. Relevant CI web, lint, audit, Linux and release-contract checks passed. Local main and both production /opt/pigeonpost-src checkouts were verified clean at the merged implementation, including the Mac release tag. Rust service source/configuration and native app code did not change.

## Validation limits
Visual browser inspection could not run because the current Codex TUI session has no browser connection. Device behavior was tested through the actual HTML/JavaScript with jsdom; production HTTP and archive checks passed. Intel execution was tested under Rosetta on Apple silicon, not on a separate physical Intel Mac. Publication and deployment work are complete.
