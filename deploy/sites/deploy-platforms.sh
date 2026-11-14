#!/usr/bin/env bash
# Assemble and publish the signed-in platforms and the help/status sites to raijin.
#
#   deploy/sites/deploy-platforms.sh assemble <out-dir> [platform…]   build roots locally
#   deploy/sites/deploy-platforms.sh publish  <out-dir> [platform…]   stage on raijin, then one sudo -S install
#
# Platforms: account partner management enterprise help status. Each root is the platform's own
# directory (site-<platform>/) plus site-shared/ copied to /shared/ (tests, node_modules and
# package files are left out). Every install keeps a backup under /var/backups/pigeonpost/.
# The sudo password is read from stdin by the caller (never echoed): see the deploy runbook.
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
cmd="${1:?assemble|publish}"; out="${2:?out dir}"; shift 2
platforms=("$@"); [ ${#platforms[@]} -gt 0 ] || platforms=(account partner management enterprise help status)

assemble() {
  rm -rf "$out"; mkdir -p "$out"
  rev="$(git -C "$here" rev-parse --short HEAD)$(git -C "$here" diff --quiet || echo -dirty)"
  for p in "${platforms[@]}"; do
    src="$here/site-$p"; [ -d "$src" ] || { echo "missing $src" >&2; exit 1; }
    rsync -a --exclude test --exclude 'test-*' --exclude node_modules --exclude 'package*.json' --exclude '*.md' "$src/" "$out/$p/"
    mkdir -p "$out/$p/shared"
    rsync -a --exclude test --exclude node_modules --exclude 'package*.json' "$here/site-shared/" "$out/$p/shared/"
    # Cache-bust: every same-origin script and stylesheet a page loads gets ?v=<commit>, so a
    # deploy reaches browsers at once instead of after the 5-minute static cache.
    find "$out/$p" -name '*.html' -print0 | xargs -0 perl -pi -e 's{((?:src|href)="/[^"?#]+\.(?:js|css))"}{$1?v='"$rev"'"}g'
    find "$out/$p" -type f | wc -l | xargs echo "$p: files"
  done
}

publish() {
  assemble
  cat > "$out/install.sh" <<'REMOTE'
#!/usr/bin/env bash
# Runs on raijin as the deploy user; the sudo password arrives on stdin.
set -euo pipefail
ts=$(date -u +%Y%m%dT%H%M%SZ)
me=$(whoami)
IFS= read -rs PW
for p in "$@"; do
  root=/var/www/pigeonpost-$p
  # status.json is written every minute by the deploy user's pp-status.timer, so that root
  # belongs to the deploy user; every other root belongs to www-data.
  owner=www-data; [ "$p" = status ] && owner=$me
  printf '%s\n' "$PW" | sudo -S -p '' bash -c "
    set -e
    mkdir -p /var/backups/pigeonpost
    if [ -d $root ]; then tar -C $root -czf /var/backups/pigeonpost/site-$p-$ts.tgz . ; fi
    mkdir -p $root
    rsync -a --delete --exclude status.json /home/$me/stage-platforms/$p/ $root/
    chown -R $owner:www-data $root
    find $root -type d -exec chmod 755 {} +
    find $root -type f -exec chmod 644 {} +
  "
  echo "installed $p (backup site-$p-$ts.tgz)"
done
REMOTE
  # Only install.sh may read stdin (the password): keep every earlier ssh off it.
  ssh -n raijin "rm -rf ~/stage-platforms && mkdir -p ~/stage-platforms"
  rsync -az "$out/" raijin:stage-platforms/ </dev/null
  # stdin (the sudo password) passes through ssh to install.sh's read.
  ssh raijin "bash ~/stage-platforms/install.sh ${platforms[*]}"
}

case "$cmd" in
  assemble) assemble ;;
  publish) publish ;;
  *) echo "usage: $0 assemble|publish <out-dir> [platform…]" >&2; exit 2 ;;
esac
