# Agent SSH: tunnel broker and SSH CA runbook (docs/agent-ssh.md)

Path: `wss://tunnel.pigeonpost.dev` → Cloudflare → wodomini nginx (TLS, `nginx-wodomini-tunnel.conf`)
→ raijin nginx `:48880` (`nginx-raijin-tunnel.conf`) → `pigeonpost-tunnel` on `127.0.0.1:8796`
→ postbox internal API through the existing `pigeonpost-postbox-tunnel.service` (`127.0.0.1:18991`).

## Build (raijin has Docker, no Rust toolchain)

```sh
# from a clean checkout of the merged commit, staged in ~/pp-tunnel-src on raijin
docker run --rm -v "$PWD":/src -w /src docker.io/library/rust:1.91.1-bookworm \
  cargo build --release --locked -p pigeonpost-tunnel
sha256sum target/release/pigeonpost-tunnel
```

## Install (one `sudo -S` on raijin)

1. Back up to `/var/backups/pigeonpost/p8-tunnel-<ts>/`: `nginx.conf`, any existing unit/env.
2. `install -D -m 0755 target/release/pigeonpost-tunnel /opt/pigeonpost-tunnel/pigeonpost-tunnel`
3. `/etc/pigeonpost-tunnel.env` (0600 root) from `tunnel.env.example`; the token is generated on the
   postbox host (`svc_` + 64 hex) and copied host-to-host, never through a workstation or a log.
4. `install -m 0644 pigeonpost-tunnel.service /etc/systemd/system/`, `systemctl daemon-reload`,
   `systemctl enable --now pigeonpost-tunnel`.
5. Add the server block to `/etc/nginx-raijin-webapps/nginx.conf`, `nginx -t`, reload.

wodomini: install `nginx-wodomini-tunnel.conf` with the port-80 block first, issue the certificate
(`certbot certonly --webroot -w /var/www/certbot -d tunnel.pigeonpost.dev`), then the TLS block,
`nginx -t`, reload.

## SSH CA (postbox host, 45.132.181.100)

```sh
install -d -m 0700 /opt/pigeonpost-postbox/ssh-ca
ssh-keygen -q -t ed25519 -N '' -C pigeonpost-ssh-ca -f /opt/pigeonpost-postbox/ssh-ca/ca
chown 65532:1000 /opt/pigeonpost-postbox/ssh-ca/ca && chmod 0400 /opt/pigeonpost-postbox/ssh-ca/ca
rm /opt/pigeonpost-postbox/ssh-ca/ca.pub     # the postbox derives and publishes the public key
```

Mount it read-only into the postbox container (`/opt/pigeonpost-postbox/ssh-ca:/run/ssh-ca:ro`)
and add to `postbox.env`: `POSTBOX_SSH_CA_KEY=/run/ssh-ca/ca`,
`POSTBOX_SHELL_BROKER_URL=wss://tunnel.pigeonpost.dev`, `POSTBOX_SVC_TOKEN_TUNNEL=svc_…`.
The key is never printed, copied off the host, or included in backups that leave it.

## Verify

* `curl -s https://tunnel.pigeonpost.dev/healthz` → `{"status":"ok",…}`; `/t/<bad>` → 404.
* `GET https://postbox.pigeonpost.dev/v1/shell/ca` → the CA public key and fingerprint.
* Internal: the tunnel token reaches `/internal/v1/health` and nothing outside `shell/*`.

## Rollback

1. `agent_ssh` flag off (ops token): `PUT /internal/v1/features/agent_ssh {"scope":"global","enabled":false}`.
2. `systemctl disable --now pigeonpost-tunnel` (ends every live session).
3. Restore the backed-up nginx configs and reload; restore the postbox `compose.json`/`postbox.env`
   from the deploy backup and `docker compose up -d`. The new tables are additive.
