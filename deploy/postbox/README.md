# pigeonpost-postbox — hosted plane

The Dockerized hosted plane (remote MCP + key custody + inbox hosting) for mass adoption.
Design: [`docs/planning/hosted-postbox-architecture-2026-08-12.md`](../../docs/planning/hosted-postbox-architecture-2026-08-12.md) *(gitignored planning doc)*.

**Status: P0 in progress.** Live: proof-of-work anti-abuse (`GET /v1/pow/challenge`); PoW-gated
anonymous `/k/` identity creation (`POST /v1/identities`) — mints a keypair, seals the seed in the
vault, persists to SQLite, returns a capability token; the capability-token-authed messaging loop
(`POST /v1/send` hosted→hosted, `GET /v1/inbox`, `POST /v1/ack`); and the **MCP connector**
(`POST /mcp`, JSON-RPC) exposing `whoami` / `send_pigeonpost_message` / `check_pigeonpost_inbox` /
`ack_pigeonpost_message`. Point a Claude/ChatGPT client at `https://mcp.pigeonpost.dev/mcp` with the
capability token as its bearer. Not yet built: cross-box delivery, accounts/OAuth, quotas, Postgres.

## Host

- Box: **`159.69.201.24`** — `postbox.pigeonpost.dev` (A) + `mcp.pigeonpost.dev` (CNAME), both
  **grey-cloud** (not Cloudflare-proxied) so MCP streaming isn't cut and Caddy can auto-issue TLS.
- Firewall the box to **80/443 only**; rely on PoW + app rate limits for abuse (no edge WAF on grey).

## Bring it up

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
