#!/usr/bin/env bash
# Publish the status page and the outside monitor to nests-1 (ssh alias `nests`, root), the host
# that serves status.pigeonpost.dev (wodomini proxies there; raijin keeps only the fallback copy).
#
#   deploy/status/deploy-outside.sh page [assembled-dir]   site-status/ (+ site-shared as /shared/) -> /var/www/pigeonpost-status
#   deploy/status/deploy-outside.sh monitor                pp-status.py + units -> /usr/local/lib/pp-status, /etc/systemd/system
#
# `deploy/sites/deploy-platforms.sh publish <out> status` calls `page <out>/status` itself, so the
# normal platform deploy reaches nests-1 and raijin together. Both refuse a tree with uncommitted
# changes (prod serves merged commits only); PP_DEPLOY_ALLOW_DIRTY=1 overrides for emergencies.
# Every replaced tree or file is backed up under /var/backups/pigeonpost/ on nests-1 first.
# First-time host setup (user, nginx, certificate, SSH ingest key) is in deploy/status/README.md.
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
host="${PP_STATUS_HOST:-nests}"
ts=$(date -u +%Y%m%dT%H%M%SZ)

clean_or_die() {
  if [ "${PP_DEPLOY_ALLOW_DIRTY:-}" != 1 ] && { ! git -C "$here" diff --quiet HEAD || [ -n "$(git -C "$here" status --porcelain)" ]; }; then
    echo "refusing to deploy uncommitted changes (commit and merge first, or PP_DEPLOY_ALLOW_DIRTY=1)" >&2
    exit 1
  fi
}

case "${1:-}" in
  page)
    if [ -n "${2:-}" ]; then
      src="$2"; tmp=""
    else
      clean_or_die
      tmp="$(mktemp -d)"; src="$tmp/status"
      "$here/deploy/sites/deploy-platforms.sh" assemble "$tmp" status
    fi
    [ -f "$src/index.html" ] || { echo "no assembled status page in $src" >&2; exit 1; }
    ssh -n "$host" "mkdir -p /var/backups/pigeonpost /var/www/pigeonpost-status && tar -C /var/www/pigeonpost-status -czf /var/backups/pigeonpost/site-status-$ts.tgz . && rm -rf /root/stage-status"
    rsync -az "$src/" "$host:/root/stage-status/" </dev/null
    ssh -n "$host" "rsync -a --delete --exclude status.json /root/stage-status/ /var/www/pigeonpost-status/ && chown -R root:root /var/www/pigeonpost-status && find /var/www/pigeonpost-status -type d -exec chmod 755 {} + && find /var/www/pigeonpost-status -type f -exec chmod 644 {} +"
    [ -z "$tmp" ] || rm -rf "$tmp"
    echo "installed status on $host (backup site-status-$ts.tgz)"
    ;;
  monitor)
    clean_or_die
    ssh -n "$host" "mkdir -p /var/backups/pigeonpost/pp-status-$ts /usr/local/lib/pp-status && cp -a /usr/local/lib/pp-status/. /etc/systemd/system/pp-status-outside.* /var/backups/pigeonpost/pp-status-$ts/ 2>/dev/null || true"
    rsync -a "$here/deploy/status/pp-status.py" "$host:/usr/local/lib/pp-status/pp-status.py" </dev/null
    rsync -a "$here/deploy/status/pp-status-outside.service" "$here/deploy/status/pp-status-outside.timer" "$host:/etc/systemd/system/" </dev/null
    ssh -n "$host" "chown root:root /usr/local/lib/pp-status/pp-status.py && chmod 0755 /usr/local/lib/pp-status/pp-status.py && systemctl daemon-reload && systemctl enable --now pp-status-outside.timer"
    echo "monitor installed (backup pp-status-$ts)"
    ;;
  *) echo "usage: $0 page [assembled-dir]|monitor" >&2; exit 2 ;;
esac
