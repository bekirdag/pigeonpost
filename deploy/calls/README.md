# Web calls — deployment

Voice and video calls in the web inbox: one-to-one from a conversation, or up to six people via a
meeting link (`inbox.pigeonpost.dev/ABC234`, served by `join.html`). Three parts:

| Part | Where | What it does |
| --- | --- | --- |
| `site-inbox/calls.js`, `join.html` | the inbox's static files | UI, one `RTCPeerConnection` per pair of people, media, chat, security codes |
| `calls-service/` | next to the inbox, behind `/calls/` on the same origin | rings mailboxes, relays offer/answer/ICE, mints TURN credentials |
| coturn | a dedicated relay host | forwards encrypted media when two browsers cannot reach each other directly |

Group calls are a mesh: every pair of browsers has its own connection, so media is DTLS-SRTP end to
end between each pair and the upload budget is divided between them. Anyone with a meeting link can
ask to join (a guest name, or a verified mailbox if signed in); somebody already in the call admits
them. Media is DTLS-SRTP between browsers; the service and the relay only see ciphertext. Each
screen shows a six-digit security code computed from both browsers' DTLS certificate fingerprints;
matching codes mean the signaling path did not substitute a certificate. The service decides
nothing about identity itself: it asks the postbox (`/v1/identities`, `/v1/whoami`) which mailboxes
the caller's own token owns. Busy and offline both look like "no answer" to the caller.

Budget: Opus up to 128 kbit/s plus video capped at 3.8 Mbit/s ≈ 4 Mbit/s of media per sender,
before RTP/UDP/IP (and TURN) overhead. Video adapts down on weaker links; 1080p is not guaranteed.

## Relay

1. `apt-get install coturn ufw`.
2. `openssl rand -hex 32 > /etc/pigeonpost-turn/secret` (0600), then write `/etc/turnserver.conf`
   from `turnserver.conf.example`, substituting the relay IP and that secret on the host.
3. Firewall: SSH, 3478/udp+tcp, 5349/tcp, 49160–59999/udp. Allow SSH before enabling ufw.
4. Check: an allocation with a fresh HMAC credential succeeds; a wrong or expired one fails
   (`turnutils_uclient -t -u <expiry>:probe -w <hmac> <ip>`).
5. TURN over TLS (5349, or 443) needs a DNS-only hostname and a certificate; add `cert`, `pkey` and
   `tls-listening-port` once that record exists.

## Service

1. `useradd --system --no-create-home pigeonpost-calls`; copy `calls-service/src` to `/opt/pigeonpost-calls/src`.
2. `/etc/pigeonpost-calls/calls.env` from `calls.env.example` (0640 root:pigeonpost-calls), with the
   relay's secret copied host to host — never through the repository.
3. Install `pigeonpost-calls.service`, `systemctl enable --now pigeonpost-calls`.
4. Add both blocks of `nginx-inbox-calls.conf` inside the inbox server block, `nginx -t`, reload.
5. `curl https://inbox.pigeonpost.dev/calls/v1/health` → `{"ok":true,…}`.

Deploy the service before the inbox files: without `/calls/` the inbox simply hides the call buttons.

## Rollback

Restore the inbox files from their backup (the buttons disappear), then `systemctl disable --now
pigeonpost-calls` and remove the nginx location. The relay can stay; it accepts nothing without a
credential from the service.

## Testing

`cd calls-service && npm test` covers authorization, ringing, busy/decline/cancel/timeout, signal
validation, resume and TURN credentials. Two-browser checks should use synthetic media and force the
relay (`localStorage.ppi_calls_relay = "1"`) and read `PigeonpostCalls.inspect()` for the selected
candidate pair, resolution, bitrate and security code on both ends.
