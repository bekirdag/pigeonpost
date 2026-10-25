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

## Remaining

Finish signaling qualification and deploy the source correction with the verified TLS URLs, then verify the actual running environment, public browser health and preserved call-service settings. Apple and other store submissions remain held.
