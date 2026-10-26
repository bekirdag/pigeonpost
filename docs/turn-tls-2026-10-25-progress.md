# TURN TLS progress

## Verified before deployment

- Owner confirmed Cloudflare sign-in on 25 October 2026.
- Added `turn.pigeonpost.dev` A record pointing at the dedicated media host, DNS only. Both authoritative nameservers and a public resolver return the relay address.
- `enterprise.pigeonpost.dev` already resolves and serves HTTPS successfully.
- Coturn 4.6.1 supports alternate TLS listeners and SIGUSR2 certificate reload. The existing service runs as `turnserver`; 80 and 443 are unused. Existing 3478 listeners, relay range, LiveKit ports and restricted 7443 backhaul must be preserved.
- No live relay allocations or established TURN control connections at the initial audit; recheck immediately before applying listeners.

## Relay deployed and verified

- Certbot issued a public ECDSA certificate, valid through 23 January 2027. Both public ports, 5349 and 443, pass hostname and trust verification.
- Coturn's RFC5780 alternate listener is disabled on this single-public-IP host. An initial attempt detected the missing 443 listener and rolled back. Native auxiliary listener 443 passes both TLS and actual relay checks; no new proxy was required.
- Configuration, firewall and service metadata are backed up on the relay under `/var/backups/pigeonpost/turn-tls-20261025T202104Z`. Authentication, 3478 transports and the LiveKit backhaul are preserved.
- External probes on each TLS port created two authenticated allocations, verified response integrity and ten packets in each direction with exact payloads, rejected an invalid password, then released the allocations.
- Installed the root-owned renewal hook and enabled Certbot's timer. A second external probe invoked the real hook while allocations stayed open: the PID and existing connections survived and media continued in both directions. Certbot renewal dry run including deploy hooks passed.
- Seven isolated negative hook checks passed: mismatched key, invalid certificate, oversized input, service replacement, post-replace publication failure, signal failure and TLS probe failure. Failure paths preserved the previous generation; the live service was untouched by these negative checks.
- Certificate directories are 0750 root:turnserver; the private key is 0640 root:turnserver. No production key or configuration backup was copied to the workstation.

## Signaling shutdown correction

The first TLS URL update encountered the existing shutdown hang: HTTP close waited indefinitely for an open event response. The script restored the original environment; the service is healthy on its previous URLs. Two real child-process regression tests reproduced the hang under SIGTERM and SIGINT. Closing the event response and its socket after flushing fixes both; an idempotent five-second deadline bounds other stuck requests. Both focused checks pass in under a tenth of a second each. The full calls-service suite passes all 63 checks. The corrected rollout is still in progress.

Docdex mapped the calls-service imports and seven inbound test/harness callers before edits. The deployment templates and standalone certificate hook have no indexed dependency edges. The generic Docdex test runner selected Cargo and rejected the JavaScript target, so Node checks use the scoped Node runner and the package's existing test command.

## Signaling deployed and verified

- Deployed the qualified shutdown correction and both TLS URLs on Raijin. The running process contains all four UDP/TCP/TLS endpoints; all other environment bytes are preserved. Source SHA-256 is `30cb15c229ac8f515c18aa2933187ba58e715d5239d07e5eba107d64af8ebc74`.
- Server-only rollback backup: `/var/backups/pigeonpost/calls-turn-tls-20261025T205439Z`. The initial legacy process still had the old shutdown handler and needed targeted termination after it began stopping; a pidfd prevented signaling a replacement process. No rooms or queued history events existed.
- Public health from the persistent Chrome browser passes: postbox integration, large calls and push ringing are configured, with no failed admission or queued history events. A one-time challenge confirms the loaded signaling secret matches the relay without copying either long-lived secret.
- PR #108 passed all 16 GitHub checks, including both Windows architectures and all platform tests, before merge. The production source was separately qualified with all 63 Node checks and exact source/environment verification.
- Native Apple candidate #106 passed all 19 checks and merged separately. Apple and other store submissions remain held; physical-device call checks are still release gates.

The DNS and relay TLS prerequisite is complete. Certbot renewal is enabled. Backups, keys and operational credential probes remain private on the servers or in the workstation's private validation directory; none are published in this repository.
