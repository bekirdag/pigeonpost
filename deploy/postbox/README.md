# pigeonpost-postbox — hosted plane

The Dockerized hosted plane (remote MCP + key custody + inbox hosting) for mass adoption.
Design: [`docs/planning/hosted-postbox-architecture-2026-08-12.md`](../../docs/planning/hosted-postbox-architecture-2026-08-12.md) *(gitignored planning doc)*.

**Status: scaffold.** The binary stands up config, logging, graceful shutdown, a container
healthcheck, the reaper entrypoint, and the HTTP surface — but `/mcp` and `/v1/*` return
`501 Not Implemented`. The P0 logic (anonymous `/k/` creation with proof-of-work, `send`/`inbox`/
`read`, key vault, accounts) is not built yet.

## Host

- Box: **`159.69.201.24`** — `postbox.pigeonpost.dev` (A) + `mcp.pigeonpost.dev` (CNAME), both
  **grey-cloud** (not Cloudflare-proxied) so MCP streaming isn't cut and Caddy can auto-issue TLS.
- Firewall the box to **80/443 only**; rely on PoW + app rate limits for abuse (no edge WAF on grey).

## Bring it up

```sh
# on 159.69.201.24, in this directory
cp postbox.env.example postbox.env      # edit; chmod 600 postbox.env
mkdir -p secrets
printf '%s' 'STRONG_DB_PASSWORD'  > secrets/pg_pw       # must match POSTBOX_DB_URL
# place the sealed KMS master key (P2); a placeholder is fine for the scaffold:
:                                        > secrets/master.age
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
| `docker-compose.yml` | five-container stack: caddy · postbox · postgres · reaper · backup |
| `Caddyfile` | TLS + reverse proxy for both hostnames; no-buffer for MCP streaming |
| `postbox.env.example` | env template (copy to `postbox.env`, never commit the real one) |

## Local dev (no Docker)

```sh
cargo run -p pigeonpost-postbox            # serves :8990 with stub routes
cargo run -p pigeonpost-postbox -- --reaper
cargo test -p pigeonpost-postbox
```
