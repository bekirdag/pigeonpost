#!/usr/bin/env bash
# Publish the status page and the outside monitor to nests-1 (ssh alias `nests`, root).
#
#   deploy/status/deploy-outside.sh page      site-status/ (+ site-shared as /shared/) -> /var/www/pigeonpost-status
#   deploy/status/deploy-outside.sh monitor   pp-status.py + units -> /usr/local/lib/pp-status, /etc/systemd/system
#
# Every replaced tree or file is backed up under /var/backups/pigeonpost/ on nests-1 first.
# First-time host setup (user, nginx, certificate, SSH ingest key) is in deploy/status/README.md.
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
host="${PP_STATUS_HOST:-nests}"
ts=$(date -u +%Y%m%dT%H%M%SZ)

case "${1:-}" in
  page)
    out="$(mktemp -d)"
    "$here/deploy/sites/deploy-platforms.sh" assemble "$out" status
    ssh -n "$host" "mkdir -p /var/backups/pigeonpost /var/www/pigeonpost-status && tar -C /var/www/pigeonpost-status -czf /var/backups/pigeonpost/site-status-$ts.tgz . && rm -rf /root/stage-status"
    rsync -az "$out/status/" "$host:/root/stage-status/"
    ssh -n "$host" "rsync -a --delete --exclude status.json /root/stage-status/ /var/www/pigeonpost-status/ && chown -R root:root /var/www/pigeonpost-status && find /var/www/pigeonpost-status -type d -exec chmod 755 {} + && find /var/www/pigeonpost-status -type f -exec chmod 644 {} +"
    rm -rf "$out"
    echo "page installed (backup site-status-$ts.tgz)"
    ;;
  monitor)
    ssh -n "$host" "mkdir -p /var/backups/pigeonpost/pp-status-$ts /usr/local/lib/pp-status && cp -a /usr/local/lib/pp-status/. /etc/systemd/system/pp-status-outside.* /var/backups/pigeonpost/pp-status-$ts/ 2>/dev/null || true"
    rsync -a "$here/deploy/status/pp-status.py" "$host:/usr/local/lib/pp-status/pp-status.py"
    rsync -a "$here/deploy/status/pp-status-outside.service" "$here/deploy/status/pp-status-outside.timer" "$host:/etc/systemd/system/"
    ssh -n "$host" "chown root:root /usr/local/lib/pp-status/pp-status.py && chmod 0755 /usr/local/lib/pp-status/pp-status.py && systemctl daemon-reload && systemctl enable --now pp-status-outside.timer"
    echo "monitor installed (backup pp-status-$ts)"
    ;;
  *) echo "usage: $0 page|monitor" >&2; exit 2 ;;
esac
