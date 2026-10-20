# Web calls — deployment

Voice and video calls in the web inbox: one-to-one from a conversation, or a group via a meeting
link (`inbox.pigeonpost.dev/ABC234`, served by `join.html`). Up to six people talk over a mesh; from
the seventh up to 30, the call runs through a media server with end-to-end frame encryption (see
"Media server" below and `docs/calls-sfu.md`). Four parts:

| Part | Where | What it does |
| --- | --- | --- |
| `site-inbox/calls.js`, `join.html` | the inbox's static files | UI, one `RTCPeerConnection` per pair of people, media, chat, security codes |
| `calls-service/` | next to the inbox, behind `/calls/` on the same origin | rings mailboxes, relays offer/answer/ICE, mints TURN credentials |
| coturn | a dedicated relay host | forwards encrypted media when two browsers cannot reach each other directly |
| LiveKit (`livekit.service`) | the same media host, 193.164.5.36 | forwards frames the browsers already encrypted, for calls of 7–30 |

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

1. `useradd --system --no-create-home pigeonpost-calls`; copy `calls-service/src` (all files, including
   `postbox-internal.js`) to `/opt/pigeonpost-calls/src`.
2. `/etc/pigeonpost-calls/calls.env` from `calls.env.example` (0640 root:pigeonpost-calls), with the
   relay's secret copied host to host — never through the repository.
3. Install `pigeonpost-calls.service`, `systemctl enable --now pigeonpost-calls`.
4. Add both blocks of `nginx-inbox-calls.conf` inside the inbox server block, `nginx -t`, reload.
5. `curl https://inbox.pigeonpost.dev/calls/v1/health` → `{"ok":true,…}`.

Deploy the service before the inbox files: without `/calls/` the inbox simply hides the call buttons.

## Call admission and call history (Phase 1b)

Before ringing, the service asks the postbox whether the callee's settings say ring, silence or
block (`POST /internal/v1/call-admission`), and it reports each call's lifecycle (ring, answer, join,
decline/busy, missed, cancelled, end, filtered) to `POST /internal/v1/call-events` so both sides get
a call history. Both are on the postbox's **internal** listener (`127.0.0.1:8991` on the postbox host
45.132.181.100), which is not public. raijin reaches it through the private tunnel described in
`deploy/postbox/` (it listens on raijin at `127.0.0.1:18991`).

| Variable | Value |
| --- | --- |
| `POSTBOX_INTERNAL_URL` | `http://127.0.0.1:18991` (the tunnel's local end) |
| `POSTBOX_SERVICE_TOKEN` | the calls service token (`svc_…`), equal to `POSTBOX_SVC_TOKEN_CALLS` in the postbox's `postbox.env`; copy host to host |

Behaviour:

- Unset: rings without asking and reports nothing (the pre-1b behaviour).
- Admission answers are cached for 30 s per caller, callee and kind. If the postbox is configured
  but cannot answer within 3 s, admission **fails closed**: nothing rings and the caller sees the
  ordinary no-answer.
- `block` and `silence` are indistinguishable from a genuine no-answer for the caller: same response,
  same events, same timing, and the caller's own history shows an unanswered call. `silence` files a
  "filtered" entry in the callee's history; `block` records nothing for the callee.
- Events go through a bounded in-memory retry queue (1,000 events, oldest dropped when full) with a
  stable `event_id` per event, exponential backoff from 1 s to 60 s and up to 12 attempts (or 30
  minutes). A 4xx other than 429 is dropped as rejected. The queue is not durable: a restart loses
  whatever is queued.
- `GET /calls/v1/health` includes `postbox: {configured, queued, sent, retried, dropped, rejected,
  failed, admission_ok, admission_failed, cache_hits}` — counts only. A growing `dropped` or
  `admission_failed` means the tunnel or the postbox is down.

Check after deploying: `curl -s https://inbox.pigeonpost.dev/calls/v1/health` shows
`"configured":true`, and `admission_failed` stays at 0 across a test call.

## Media server (calls of 7–30)

Pinned: `livekit-server` 1.13.9 (linux amd64, sha256
`0b7fa208b662d09cfdeae8c06cf4c481aead0556086558b48250501e2e2d6e20`, checked against the release's
`checksums.txt` and GitHub's asset digest) in `/opt/livekit/1.13.9`, linked from `/opt/livekit/current`.
Client: `site-inbox/vendor/livekit-client` 2.22.4, pinned by hash.

How a browser reaches it:

- **Signaling:** `wss://inbox.pigeonpost.dev/sfu/…`, on the inbox's own origin, so the inbox CSP is
  unchanged. The path is Cloudflare, then wodomini (`location /sfu/`, no access log), then raijin
  (`nginx-inbox-sfu.conf`), then the media host (`nginx-sfu-backhaul.conf`, TLS on 7443) and LiveKit
  on `127.0.0.1:7880`. raijin trusts exactly one self-signed certificate on that last hop
  (`sfu-backhaul.pigeonpost.internal`, ECDSA P-256, valid to 2036). ufw admits 7443 only from
  95.70.192.142. `/sfu/twirp/` is refused publicly.
- **Media:** straight to the media host: UDP 40000–41999, and TCP 7881 for networks that block UDP.
  This range does not overlap coturn's relay range, 49160–59999. The coturn credentials are offered
  too, for networks that only allow TURN.

Install (media host, root): back up `/etc/ufw` and `/etc/nginx`, `useradd --system livekit`, unpack
and verify the release, then install the files from this directory:

- `livekit.yaml` → `/etc/livekit/livekit.yaml` (0640 root:livekit);
- `livekit.service` → `/etc/systemd/system/`;
- `nginx-sfu-backhaul.conf` → `/etc/nginx/sites-enabled/livekit-backhaul`.

Replace `MEDIA_HOST_IP` in those files. Generate on the host
`/etc/livekit/livekit.env` (0600) with `LIVEKIT_KEYS="<key>: <secret>"` (a 64-character random
secret), and the backhaul certificate (`openssl req -x509 -newkey ec -pkeyopt
ec_paramgen_curve:P-256 -days 3650 -nodes -subj /CN=sfu-backhaul.pigeonpost.internal -addext
subjectAltName=DNS:sfu-backhaul.pigeonpost.internal`). Then open the firewall:

    ufw allow from <raijin public IP> to any port 7443 proto tcp
    ufw allow 7881/tcp
    ufw allow 40000:41999/udp

Finally `systemctl enable --now livekit nginx`.

On raijin, copy the backhaul certificate to `/etc/nginx-raijin-webapps/sfu-backhaul.crt` and
`/etc/pigeonpost-calls/sfu-backhaul.crt`, add `nginx-inbox-sfu.conf` next to `location /calls/`, and
add the `CALLS_SFU_*` lines of `calls.env.example` to `calls.env`. Copy the key and secret host to
host, never through the repository or a log. Restart `pigeonpost-calls`. The service logs `large
calls on the media server, budget N Mbit/s` when it is enabled.

**DNS (optional, later):** a DNS-only (grey cloud) record `media.pigeonpost.dev A 193.164.5.36` would
let browsers reach signaling directly (`CALLS_SFU_URL=wss://media.pigeonpost.dev`, with a certificate
on the host, and `connect-src` extended in the inbox CSP). It is not needed: the route through the
inbox origin above is the production path. Today `*.pigeonpost.dev` resolves to Cloudflare's proxy.

### Capacity

The calls service admits a large room, or one more person into it, only while the expected upload of
all large rooms stays within `CALLS_SFU_BUDGET_MBPS`. Otherwise it refuses with `call_busy` ("The
call is busy … try again in a few minutes"). Per receiver it counts one large view at 1.7 Mbit/s, up
to 15 thumbnails at 0.25 Mbit/s each, and 0.06 Mbit/s per other voice (`sfuRoomMbps`). These are the
values measured on the production media server with 30 synthetic participants on 2026-10-20: it
forwarded 160–224 Mbit/s and received 18–28 Mbit/s, at a load average of about 3 on 5 vCPUs. One
30-person room plans ≈216 Mbit/s, one 12-person room ≈58 Mbit/s.

Measured on 2026-10-20 with iperf3, 4 streams, 15 s, from the media host:

- to `iperf-ams-nl.eranium.net`: 2.0 Gbit/s up, 1.33–1.40 Gbit/s down;
- to `speedtest.wtnet.de`: 1.8 Gbit/s up, 0.27–0.32 Gbit/s down.

The budget is set to 400 Mbit/s: about 20% of the measured upload, which leaves headroom for coturn
and for paths slower than these test servers. That is one full 30-person call next to smaller ones,
or about six calls of 12. CPU is the next limit: one 30-person call put the 5-vCPU host at a load of
about 3. Raise it only after watching real calls: `ss`/`nload` on the host, LiveKit's CPU.

Clients publish three simulcast layers (180p, 360p, top at most 1.7 Mbit/s; VP8). A receiver gets
the top layer only for the view on the stage. Some WebKit clients (the macOS app) publish a single
360p layer instead (`docs/calls-sfu.md`).

### Rotating the media-server secret

Generate a new `LIVEKIT_KEYS` on the media host, copy the key and secret to `calls.env` on raijin,
then restart `livekit` and `pigeonpost-calls`. Calls in progress on the media server drop and have
to be rejoined.

## Rollback

Restore the inbox files from their backup (the buttons disappear), then `systemctl disable --now
pigeonpost-calls` and remove the nginx location. The relay can stay; it accepts nothing without a
credential from the service.

To turn off large calls only, empty `CALLS_SFU_URL` in `calls.env` and restart `pigeonpost-calls`.
Calls are then capped at six again. To remove the media server, also run `systemctl disable --now
livekit nginx` on the media host and restore `/root/backup-<time>/ufw`.

## Testing

`cd calls-service && npm test` covers authorization, ringing, busy/decline/cancel/timeout, signal
validation, resume and TURN credentials, plus (`test/postbox-internal.test.mjs`) admission decisions,
the no-answer equivalence for blocked and silenced calls, the 30 s cache, failing closed, event
reporting order and the retry queue. Two-browser checks should use synthetic media and force the

validation, resume and TURN credentials. It also covers large calls: the move at seven, the limit
of 30, the frame-encryption requirement, the bandwidth budget, media-server tickets, key
announcements and wrapped-key signals, and removal from the media server.
`site-inbox/test/calls-e2ee.test.mjs` covers the key wrapping, the security code, sealed chat and
the vendored client's hashes. End-to-end checks of large calls (a local `livekit-server`, synthetic
media) should confirm three things. Every page agrees on `PigeonpostCalls.inspect().sfu.code` and
`.epoch`. `PigeonpostCalls.sfuStats()` shows decoded audio and video from everyone. A client with a
wrong key decodes nothing. Two-browser checks should use synthetic media and force the
relay (`localStorage.ppi_calls_relay = "1"`) and read `PigeonpostCalls.inspect()` for the selected
candidate pair, resolution, bitrate and security code on both ends.
