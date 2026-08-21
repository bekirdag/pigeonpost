# pigeonpost-postbox — hosted plane

The Dockerized hosted plane (remote MCP + key custody + inbox hosting) for mass adoption.
Design: [`docs/planning/hosted-postbox-architecture-2026-08-12.md`](../../docs/planning/hosted-postbox-architecture-2026-08-12.md) *(gitignored planning doc)*.

**Status: P0 live** at `https://postbox.pigeonpost.dev` / `https://mcp.pigeonpost.dev`. Proof-of-work
anti-abuse (`GET /v1/pow/challenge`); PoW-gated
anonymous `/k/` identity creation (`POST /v1/identities`) — mints a keypair, seals the seed in the
vault, persists to SQLite, returns a capability token; the capability-token-authed messaging loop
(`POST /v1/send` hosted→hosted, `GET /v1/inbox`, `POST /v1/ack`); and the **MCP connector**
(`POST /mcp`, JSON-RPC) exposing `whoami` / `send_pigeonpost_message` / `check_pigeonpost_inbox` /
`ack_pigeonpost_message` / `list_pigeonpost_threads` / `read_pigeonpost_thread`. Point a Claude/ChatGPT client at `https://mcp.pigeonpost.dev/mcp` with the
capability token as its bearer. Not yet built: cross-box delivery, accounts/OAuth, quotas, Postgres.

## Host

- Box: **`159.69.201.24`** — `postbox.pigeonpost.dev` (A) + `mcp.pigeonpost.dev` (CNAME), both
  **grey-cloud** (not Cloudflare-proxied) so MCP streaming isn't cut and Caddy can auto-issue TLS.
- Both grey-cloud so MCP streaming isn't cut by an edge proxy.

## Production deploy (as actually live — 2026-08-12)

`159.69.201.24` is a **shared box** (`web`) already running Apache on 80/443 (many vhosts + Drone +
the pigeonpost loft) with old Docker 18.09 and no `docker compose`. So production does **not** use
the Compose/Caddy stack below — it mirrors the loft's proven pattern: a loopback container behind an
Apache vhost with certbot TLS. The Compose stack stays valid for a *dedicated* box.

Live setup (SSH `-p 34251 root@159.69.201.24`):
- Source at `/opt/pigeonpost-src` (rsync of `crates/` + manifests + `deploy/`); runtime state at
  `/opt/pigeonpost-postbox` (`postbox.env`, `master.age`, `data/`) owned by uid `65532`.
- Image built on the box (**bullseye base** — 18.09's seccomp blocks bookworm's `clone3`):
  `docker build -f deploy/postbox/Dockerfile -t pigeonpost-postbox:0.2.0 .`
- Two containers, `--restart unless-stopped`: `pigeonpost-postbox` (`-p 127.0.0.1:8990:8990`) and
  `pigeonpost-postbox-reaper` (`--reaper`), sharing `/opt/pigeonpost-postbox/data`.
- Apache vhost `pigeonpost-postbox.conf` proxies `postbox.` + `mcp.pigeonpost.dev` → `127.0.0.1:8990`
  (always `apache2ctl configtest` before reload); `certbot --apache` issued the cert + HTTP→HTTPS.
- **Redeploy:** rsync the tree, rebuild the image, `docker rm -f` + re-`docker run` both containers.
  Data and TLS persist.

## Push notifications (APNs)

Off unless configured, and configured only from the environment — no key is ever committed, and
none is compiled in. Add to `/opt/pigeonpost-postbox/postbox.env` and restart the container:

```
PIGEONPOST_APNS_KEY_PATH=/data/apns.p8      # mounted into the container, chmod 600, uid 65532
PIGEONPOST_APNS_KEY_ID=XXXXXXXXXX
PIGEONPOST_APNS_TEAM_ID=<the 10-character team id>
PIGEONPOST_APNS_TOPIC=dev.pigeonpost.inbox
# PIGEONPOST_APNS_PREVIEW=0                 # metadata only: says who wrote, not what they said
```

`PIGEONPOST_APNS_KEY` takes the PEM inline instead, for hosts where mounting a file is awkward.

**The key is an APNs auth key, not the App Store Connect API key** — a different key from a
different page. Apple Developer → Certificates, Identifiers & Profiles → **Keys** → **+**, enable
**Apple Push Notifications service (APNs)**, download the `.p8` once. Its 10-character id is
`PIGEONPOST_APNS_KEY_ID`.

With none of it set the postbox logs nothing, sends nothing, and behaves exactly as it did before
push existed. `POST /v1/devices` still accepts registrations, so phones already in the field start
being woken the moment a key is added — no app update needed.

## Retention and quotas

Three tiers, and only one of them expires:

| tier | who | retention | when full |
| --- | --- | --- | --- |
| anonymous | a `/k/` mailbox minted by proof-of-work, no account | deleted `EPHEMERAL_RETENTION_DAYS` after it was created | senders refused |
| free | signed in, no live namespace | never deleted | senders refused; the app offers a handle |
| paid | holds an unexpired namespace | never deleted | senders refused |

```
FREE_QUOTA_MB=20            # ~3,000 messages at the ~6.5 KB agent traffic averages
PAID_QUOTA_MB=1024
ANONYMOUS_QUOTA_MB=5
EPHEMERAL_RETENTION_DAYS=30 # anonymous mailboxes only
```

`MAX_INBOX_MESSAGES` is gone. A message count is not a disk limit: a thousand pings and a thousand
status reports differ by two orders of magnitude, and the count cannot tell them apart. Quotas are
summed `wrap_blob` bytes over everything a mailbox holds, sent copies included.

**A full mailbox refuses the sender** — `409 recipient_inbox_full` — rather than deleting the
holder's oldest mail to make room. Evicting would lose something somebody was keeping, silently, to
make space for something they might not want. The consequence is that the bounce goes to whoever
wrote, and the holder sees nothing; `GET /v1/quota` and the app's Mailbox section exist so they are
not the last to know.

**Deleting is the escape hatch.** `POST /v1/messages/delete` removes one message from the acting
mailbox and frees its bytes. Without it a bounded, never-expiring mailbox fills once and stays full,
and subscribing would be the only way out of a mailbox full of junk — a trap rather than a trial.
Archiving is not this: it hides a conversation and keeps every byte.

**Before 0.7.1 the sweep deleted by age alone**, every identity and message older than the cutoff,
whoever owned them. Signed-in accounts were included, so a paid handle's mailboxes were scheduled
for deletion thirty days after purchase. The line is `account_id IS NULL`, which was already in the
data and simply never consulted.

## Selling handles (App Store)

Off unless configured. With no key set, `/v1/claims/apple` answers 404 — indistinguishable from
"no such route", because a deployment that cannot verify a purchase should not look like one that
will. The iOS app reads that 404 as "this postbox does not sell handles" and hides the section
rather than showing a price it cannot honour.

```
PIGEONPOST_APPSTORE_KEY_PATH=/data/appstore.p8   # mounted, chmod 600, uid 65532
PIGEONPOST_APPSTORE_KEY_ID=XXXXXXXXXX
PIGEONPOST_APPSTORE_ISSUER_ID=<the App Store Connect issuer id>
# PIGEONPOST_APPSTORE_BUNDLE_ID=dev.pigeonpost.inbox
# PIGEONPOST_APPSTORE_PRODUCT_ID=dev.pigeonpost.inbox.handle.yearly
```

`PIGEONPOST_APPSTORE_KEY` takes the PEM inline instead.

**This is a third key type again.** Apple issues three and they are not interchangeable: the APNs
key above, the App Store Connect API key used by CI to upload builds, and this one — an **In-App
Purchase key**, from App Store Connect → Users and Access → Integrations → **In-App Purchase**.
Each `.p8` can be downloaded exactly once.

The issuer id, however, *is* the ordinary App Store Connect issuer id; an In-App Purchase key does
not come with one of its own. That is not obvious from the console and was established by trying
it: a token signed with this key and that issuer is accepted by the sandbox host.

Both Apple environments are tried on every claim, production first, because a transaction id does
not record which one produced it. Expect sandbox to answer for TestFlight builds and production for
App Store ones, with no configuration distinguishing them. If both refuse the token the postbox logs
that the *credentials* were rejected rather than blaming the purchase — the two failures need
different people to fix them.

The reserved-name list applies to bought names exactly as it does to free ones: `PIGEONPOST_RESERVED_NAMES`
must be set, or the endpoint refuses everything. `support` and `admin` read as the operator whoever
paid for them.

## Bring it up (dedicated box, Compose)

```sh
# on 159.69.201.24, in this directory
cp postbox.env.example postbox.env      # edit; chmod 600 postbox.env
mkdir -p secrets
# place the sealed vault master key; a random 32+ bytes is fine (its SHA-256 becomes the key):
head -c 64 /dev/urandom          > secrets/master.age
chmod 600 secrets/*

docker compose up -d --build
```

Caddy issues TLS for both names once DNS is live. Verify:

```sh
curl -s https://postbox.pigeonpost.dev/health        # {"status":"ok",...,"stage":"scaffold"}
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://mcp.pigeonpost.dev/mcp   # 501 (scaffold)
```

Supervise across reboots with a `pigeonpost-postbox.service` systemd unit that runs
`docker compose -f $(pwd)/docker-compose.yml up`.

## Files

| File | What |
|---|---|
| `Dockerfile` | multi-stage build of the `pigeonpost-postbox` crate (Debian-slim runtime, non-root) |
| `docker-compose.yml` | four-container stack: caddy · postbox · reaper · backup (P0 storage is SQLite on the `data` volume) |
| `Caddyfile` | TLS + reverse proxy for both hostnames; no-buffer for MCP streaming |
| `postbox.env.example` | env template (copy to `postbox.env`, never commit the real one) |

## Local dev (no Docker)

```sh
cargo run -p pigeonpost-postbox            # serves :8990 with stub routes
cargo run -p pigeonpost-postbox -- --reaper
cargo test -p pigeonpost-postbox
```
