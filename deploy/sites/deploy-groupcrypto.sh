#!/usr/bin/env bash
# Deploy the web group-crypto runtime (site-inbox/vendor/groupcrypto) to the inbox origin.
#
# Layout on the server (see README "Group crypto cache compatibility"):
#   vendor/groupcrypto/bundles/<WASM_SHA256>/   immutable, one per build; the current groups.js
#                                               imports loader.js?v=<sha>, nginx maps ?v=<sha> here,
#                                               and that loader imports only ?v=<sha> names.
#   vendor/groupcrypto/{loader,integrity,...}   a mirror of the CURRENT bundle. Old cached loaders
#                                               import these unversioned names; they must be one
#                                               self-consistent, current build (never an older WASM
#                                               without later fixes - audit 2026-11-03 P0-1).
# Older bundle directories are never touched, so clients holding an older versioned loader keep
# working.
#
# Usage: deploy/sites/deploy-groupcrypto.sh [GIT_REF]      (default: origin/main)
# Env:   PP_HOST (default raijin), PP_ROOT (default /var/www/pigeonpost-inbox),
#        PP_CREDS (default ~/Documents/apps/generic/.creds, line 3 is "<label>: <sudo password>"),
#        PP_DRY_RUN=1 stops after staging and verification.
set -euo pipefail

ref="${1:-origin/main}"
host="${PP_HOST:-raijin}"
root="${PP_ROOT:-/var/www/pigeonpost-inbox}"
creds="${PP_CREDS:-$HOME/Documents/apps/generic/.creds}"
repo="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
files=(pigeonpost_groupcrypto_bg.wasm pigeonpost_groupcrypto.js pigeonpost_groupcrypto.d.ts
  pigeonpost_groupcrypto_bg.wasm.d.ts integrity.js loader.js SHA256SUMS README.md package.json)
bundle_files=(pigeonpost_groupcrypto_bg.wasm pigeonpost_groupcrypto.js integrity.js loader.js)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
commit="$(git -C "$repo" rev-parse --verify "${ref}^{commit}")"
git -C "$repo" archive "${commit}" site-inbox/vendor/groupcrypto | tar -x -C "$work"
src="$work/site-inbox/vendor/groupcrypto"

# 1. The candidate must be internally consistent before anything leaves this machine.
(cd "$src" && shasum -a 256 -c SHA256SUMS >/dev/null)
sha="$(sed -n 's/.*WASM_SHA256 = "\([0-9a-f]\{64\}\)".*/\1/p' "$src/integrity.js")"
[ -n "$sha" ] || { echo "integrity.js has no digest" >&2; exit 1; }
[ "$(shasum -a 256 "$src/pigeonpost_groupcrypto_bg.wasm" | awk '{print $1}')" = "$sha" ] \
  || { echo "wasm does not match integrity.js" >&2; exit 1; }
for m in integrity pigeonpost_groupcrypto; do
  grep -q "from './${m}.js?v=${sha}'" "$src/loader.js" \
    || { echo "loader.js does not bind ./${m}.js to ${sha}" >&2; exit 1; }
done
wasm_size="$(wc -c < "$src/pigeonpost_groupcrypto_bg.wasm" | tr -d ' ')"
[ "$wasm_size" -gt 1000000 ] || { echo "wasm is implausibly small (${wasm_size} B)" >&2; exit 1; }
(cd "$src" && shasum -a 256 "${files[@]}") > "$work/STAGE.sums"
echo "candidate ${commit:0:12}: wasm ${sha} (${wasm_size} B)"

# 2. Stage in the login user's home (writable without sudo) and verify there.
stage="pp-groupcrypto-stage-${sha:0:12}"
ssh "$host" "rm -rf ~/${stage} && mkdir -p ~/${stage}"
rsync -a "$src/" "${host}:${stage}/"
scp -q "$work/STAGE.sums" "${host}:${stage}/.stage.sums"
ssh "$host" "cd ~/${stage} && sha256sum -c --quiet .stage.sums && \
  test \$(stat -c %s pigeonpost_groupcrypto_bg.wasm) -eq ${wasm_size}"
echo "staged and verified on ${host}:~/${stage}"
echo "live before:"
ssh "$host" "cd ${root}/vendor/groupcrypto && sha256sum loader.js integrity.js pigeonpost_groupcrypto_bg.wasm && ls bundles"
if [ "${PP_DRY_RUN:-0}" = 1 ]; then echo "dry run: stopping before install"; exit 0; fi

# 3. One privileged step (sudo's credential cache does not survive separate non-tty sessions):
#    back up, install the immutable bundle, then mirror it at the unversioned names.
ts="$(date -u +%Y%m%dT%H%M%SZ)"
pw="$(sed -n '3p' "$creds" | sed 's/^[^:]*: *//')"
remote=$(cat <<SH
set -eu
v=${root}/vendor/groupcrypto
st=\$(getent passwd \$SUDO_USER | cut -d: -f6)/${stage}
cp -a "\$v" "${root}.bak-groupcrypto-${ts}"
mkdir -p "\$v/bundles"
b="\$v/bundles/${sha}"
if [ -d "\$b" ]; then
  for f in ${bundle_files[*]}; do cmp -s "\$st/\$f" "\$b/\$f" || { echo "existing bundle differs: \$f" >&2; exit 3; }; done
  echo "bundle ${sha:0:12} already installed and identical"
else
  t="\$v/bundles/.${sha}.tmp"; rm -rf "\$t"; mkdir "\$t"
  for f in ${bundle_files[*]}; do install -m 0644 -o www-data -g www-data "\$st/\$f" "\$t/\$f"; done
  chown www-data:www-data "\$t"; mv "\$t" "\$b"
  echo "bundle ${sha:0:12} installed"
fi
# Unversioned mirror: WASM and glue first, then the manifest and loader that pin them, each
# replaced atomically. A request racing the swap fails the integrity check (fail closed).
for f in ${files[*]}; do
  install -m 0644 -o www-data -g www-data "\$st/\$f" "\$v/.\$f.tmp" && mv -f "\$v/.\$f.tmp" "\$v/\$f"
done
cd "\$v" && sha256sum -c --quiet SHA256SUMS && (cd "\$b" && sha256sum -c --quiet "\$st/.stage.sums" --ignore-missing)
echo "installed; backup ${root}.bak-groupcrypto-${ts}"
SH
)
printf '%s\n' "$pw" | ssh "$host" "sudo -S -p '' sh -c $(printf '%q' "$remote")"
unset pw

# 4. Verify the origin over loopback (bypasses the edge), then the public edge.
ssh "$host" "cd ~ && rm -rf ${stage}"
check="$repo/deploy/sites/check-groupcrypto-served.py"
echo "origin check (loopback):"
ssh "$host" "for f in ${bundle_files[*]}; do for q in '' '?v=20261020-p3a' '?v=${sha}'; do \
  printf '%s%s ' \$f \"\$q\"; curl -fsS -H 'Host: inbox.pigeonpost.dev' \"http://127.0.0.1:48880/vendor/groupcrypto/\$f\$q\" | sha256sum | cut -c1-16; done; done"
echo "Purge the edge cache for the unversioned and ?v=20261020-p3a URLs, then run:"
echo "  python3 -I ${check#"$repo"/}"
