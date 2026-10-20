# Private path to the postbox internal API

The postbox serves `/internal/v1/*` on a second listener (`POSTBOX_INTERNAL_BIND`). On the postbox
host (45.132.181.100) Docker publishes it as `127.0.0.1:8991` only, so it is unreachable from the
internet. The calls-service (and later the billing bridge) on raijin reach it through an SSH tunnel:

```
raijin 127.0.0.1:18991 ──ssh (pp-tunnel key, one permitted forward)──► postbox host 127.0.0.1:8991
```

Services on raijin set `POSTBOX_INTERNAL_URL=http://127.0.0.1:18991` and present their own
`POSTBOX_SERVICE_TOKEN` (`svc_…`). The tunnel is transport only; the service token is still what
authorizes each request.

## Why SSH and not WireGuard

The internal port is published on the postbox host's loopback. With WireGuard it would have to be
published on the tunnel address too, and a Docker port bound to an interface that is not up yet at
boot makes the postbox container fail to start. An SSH local forward reaches the existing loopback
port without touching the postbox compose file, and `permitopen` limits it to that one port.

## Postbox host (45.132.181.100)

- System user `pp-tunnel`, shell `/usr/sbin/nologin`, password locked.
- `/var/lib/pp-tunnel/.ssh` (root:pp-tunnel 750) and `authorized_keys` (root:pp-tunnel 640), so the
  user cannot edit its own restrictions:

  ```
  restrict,port-forwarding,permitopen="127.0.0.1:8991",command="/bin/false" ssh-ed25519 AAAA… pp-tunnel@raijin
  ```

  `restrict` turns off every forwarding, agent, X11 and pty; `port-forwarding` re-enables local
  forwarding only, and `permitopen` narrows it to `127.0.0.1:8991`. Any other forward is refused with
  "administratively prohibited", a pty request fails, and a command gets no shell.
- No firewall change: ufw already allows 22/tcp.

## raijin

- System user `pp-tunnel` (home `/var/lib/pp-tunnel`, nologin). Key `~/.ssh/id_ed25519` (600), and
  `known_hosts` pinned to the postbox host's ed25519 key (check the fingerprint against
  `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the postbox host).
- `/etc/systemd/system/pigeonpost-postbox-tunnel.service` from
  [`pigeonpost-postbox-tunnel.service`](pigeonpost-postbox-tunnel.service), enabled
  (`systemctl enable --now`), `Restart=always`, so it comes back after a reboot or a dropped link.

## Verify

```sh
# on raijin, as root; never echo the token
T=$(grep '^POSTBOX_SERVICE_TOKEN=' /etc/pigeonpost-calls/calls.env | cut -d= -f2-)
curl -s -H "Authorization: Bearer $T" http://127.0.0.1:18991/internal/v1/health   # 200 {"service":"calls",…}
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:18991/internal/v1/health  # 401
systemctl is-enabled pigeonpost-postbox-tunnel; systemctl is-active pigeonpost-postbox-tunnel
```

## Rotate the key

Generate a new key as `pp-tunnel` on raijin, replace the key in the postbox host's
`authorized_keys` (same options), `systemctl restart pigeonpost-postbox-tunnel`.

## Rollback

On raijin: `systemctl disable --now pigeonpost-postbox-tunnel`, remove the unit file,
`systemctl daemon-reload`, and drop `POSTBOX_INTERNAL_URL` from `/etc/pigeonpost-calls/calls.env`
(a `.bak-<timestamp>` copy is kept). On the postbox host: delete
`/var/lib/pp-tunnel/.ssh/authorized_keys` (or `userdel -r pp-tunnel`). The postbox itself is
unaffected either way; without the URL the calls-service rings without admission and does not
report call history.
