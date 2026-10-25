# TURN TLS completion

Complete the outstanding relay DNS and TLS work without changing call media encryption or the existing SFU backhaul.

1. Verify Cloudflare has a DNS-only A record for `turn.pigeonpost.dev`, and verify authoritative and public DNS. Check the enterprise hostname independently.
2. Preserve the running relay configuration, firewall and service metadata in a restricted backup on the relay. Confirm idle allocations before any required initial restart.
3. Obtain a free public certificate with Certbot HTTP-01. Install restricted certificate copies for the unprivileged Coturn service. Preserve existing authentication, plain TURN transports and relay range.
4. Enable native TLS on 5349 and, if supported by the installed Coturn, alternate TLS on 443 with the bind capability. Keep LiveKit and its 7443 backhaul untouched.
5. Install a root-owned renewal hook that validates the certificate/key pair and reloads certificates with SIGUSR2. Verify renewal without restarting established allocations.
6. Verify public trust and hostname, rejected invalid credentials, and real authenticated relay traffic for the new transports. Preserve a rollback path and record actual results separately.
7. Inspect the running signaling configuration before adding TLS URLs. Restart signaling only when idle, preserving other environment values and checking public health and ICE configuration afterward.

No production secrets, private keys, raw environment files or credentials belong in this repository. No store submission is part of this change; Apple review remains held for the owner's Claude review.
