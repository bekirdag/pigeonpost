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
