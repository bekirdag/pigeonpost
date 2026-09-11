# Public Mac download

## Goal and authorization
Publish a properly signed and Apple-notarized Pigeonpost Desktop build at pigeonpost.dev/download. Support Apple silicon and Intel, recommend the appropriate option automatically, and keep manual alternatives visible. The user explicitly requested public distribution and website deployment; earlier deployment and credential authorization remains in force.

## Work
1. Verify the existing 1.0 (30) universal archive, both architectures, hardened Developer ID signature, notarization ticket and Gatekeeper acceptance. Reuse the accepted binary; do not change a signed app's contents.
2. Add a static download page with a working Mac link before JavaScript runs, visible Apple silicon/Intel alternatives, minimum macOS requirement, and simple installation instructions. Recommend the universal Mac app on macOS and the web inbox on other recognized platforms. Avoid unreliable chip guesses from browser user agents.
3. Add the page to the homepage. Keep its styles separate. Permit only its same-origin script through a narrow Nginx location include; preserve existing account routes and site security headers.
4. Add focused DOM tests for device selection, iPad desktop user agents, no-script links and visible alternatives. Add a reusable archive verification check and document publishing and rollback.
5. Publish the accepted archive at an immutable public URL, back up and deploy the page/configuration, and test a fresh download from the live URL. Verify its checksum, both code signatures, notarization and normal launch where available.
6. Commit and push changes; leave local and production checkouts clean and synced. Record evidence and remaining environment limitations separately.
7. Apply the user's supplied logo_white.png to dark themes using picture sources on the homepage, account page and new download page, preserving the regular light-theme logo.

## Deployment and rollback
The site is served from /var/www/pigeonpost on wodomini. Preserve a dated copy of changed files and the active Nginx configuration before installing. Validate nginx -t before reloading. Keep the existing CLI release's latest marker if publishing the separate macos-1.0-30 release. Keep binary artifacts outside Git and outside the static source tree so normal rsync --delete deployment cannot silently remove them.
