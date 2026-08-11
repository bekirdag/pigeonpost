#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
preflight="$repo_root/deploy/preflight.sh"
production_compose="$repo_root/deploy/compose.prod.yml"
core_compose="$repo_root/deploy/compose.core.yml"

for operator_compose in "$production_compose" "$core_compose"
do
  test "$(grep -c 'create_host_path: false' "$operator_compose")" -eq 6
  for role_path in \
    PIGEONPOST_REGISTRY_DATA_HOST_PATH \
    PIGEONPOST_DIRECTORY_DATA_HOST_PATH \
    PIGEONPOST_LOFT_DATA_HOST_PATH
  do
    test "$(grep -c "source: \${$role_path:?" "$operator_compose")" -eq 1
  done
  if grep -Eq 'pigeonpost_(registry|directory|mail):/var/lib/pigeonpost' "$operator_compose"; then
    echo 'preflight acceptance: an operator Compose file retained a role-data named volume' >&2
    exit 1
  fi
done
run_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-preflight.XXXXXX")
run_root=$(CDPATH='' cd -P -- "$run_root" && pwd -P)
trap 'rm -rf -- "$run_root"' EXIT HUP INT TERM

mkdir "$run_root/bin"
call_log="$run_root/calls.log"

# These are literal source lines for isolated stub executables, not expressions in this process.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/bin/sh' \
  'set -eu' \
  'test "${PIGEONPOST_GITHUB_CLIENT_ID+x}" != x' \
  'test "${PIGEONPOST_GITHUB_CLIENT_SECRET+x}" != x' \
  'test "${PIGEONPOST_GITHUB_CLIENT_SECRET_FILE+x}" != x' \
  'test "${PIGEONPOST_GOOGLE_CLIENT_ID+x}" != x' \
  'test "${PIGEONPOST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  'test "${PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  'printf "%s\n" "$*" >> "$PIGEONPOST_PREFLIGHT_CALL_LOG"' \
  'if [ "${1:-}" = release ] && [ "${2:-}" = view ]; then' \
  '  printf "false\t%s\t%s\n" "${PIGEONPOST_PREFLIGHT_IMMUTABLE:-true}" "$PIGEONPOST_RELEASE"' \
  'elif [ "${1:-}" = release ] && [ "${2:-}" = verify ]; then' \
  '  [ "${PIGEONPOST_PREFLIGHT_RELEASE_VERIFY:-pass}" = pass ]' \
  'elif [ "${1:-}" = release ] && [ "${2:-}" = download ]; then' \
  '  destination=' \
  '  while [ "$#" -gt 0 ]; do' \
  '    if [ "$1" = --dir ]; then shift; destination=$1; fi' \
  '    shift' \
  '  done' \
  '  test -n "$destination"' \
  '  pointer=${PIGEONPOST_PREFLIGHT_RELEASE_IMAGE:-$PIGEONPOST_IMAGE}' \
  '  printf "%s\n" "$pointer" > "$destination/pigeonpost-container.txt"' \
  '  digest=$(sha256sum "$destination/pigeonpost-container.txt" | cut -d" " -f1)' \
  '  printf "%s  pigeonpost-container.txt\n" "$digest" > "$destination/SHA256SUMS"' \
  'elif [ "${1:-}" = api ]; then' \
  '  printf "%s\n" aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' \
  'fi' \
  > "$run_root/bin/gh"
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/bin/sh' \
  'set -eu' \
  'test "${PIGEONPOST_GITHUB_CLIENT_SECRET+x}" != x' \
  'case "$*" in' \
  '  "compose -f "*" config --quiet")' \
  '    test "${PIGEONPOST_GITHUB_CLIENT_ID:-}" = "$PIGEONPOST_PREFLIGHT_EXPECTED_GITHUB_ID"' \
  '    test "${PIGEONPOST_GITHUB_CLIENT_SECRET_FILE:-}" = "$PIGEONPOST_PREFLIGHT_SECRET_FILE"' \
  '    test "${PIGEONPOST_GOOGLE_CLIENT_ID:-}" = "$PIGEONPOST_PREFLIGHT_EXPECTED_GOOGLE_ID"' \
  '    test "${PIGEONPOST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  '    test "${PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  '    ;;' \
  '  *)' \
  '    test "${PIGEONPOST_GITHUB_CLIENT_ID+x}" != x' \
  '    test "${PIGEONPOST_GITHUB_CLIENT_SECRET_FILE+x}" != x' \
  '    test "${PIGEONPOST_GOOGLE_CLIENT_ID+x}" != x' \
  '    test "${PIGEONPOST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  '    test "${PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  '    ;;' \
  'esac' \
  'printf "%s\n" "$*" >> "$PIGEONPOST_PREFLIGHT_CALL_LOG"' \
  > "$run_root/bin/docker"
# GNU-stat contract used by production preflight. The acceptance is portable and substitutes the
# container uid while preserving exact trace-directory and secret-file checks.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/bin/sh' \
  'set -eu' \
  'test "${PIGEONPOST_GITHUB_CLIENT_ID+x}" != x' \
  'test "${PIGEONPOST_GITHUB_CLIENT_SECRET+x}" != x' \
  'test "${PIGEONPOST_GITHUB_CLIENT_SECRET_FILE+x}" != x' \
  'test "${PIGEONPOST_GOOGLE_CLIENT_ID+x}" != x' \
  'test "${PIGEONPOST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  'test "${PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES+x}" != x' \
  'test "$1" = -c' \
  'format=$2' \
  'test "$3" = --' \
  'path=$4' \
  'case "$path" in' \
  '  "$PIGEONPOST_REGISTRY_DATA_HOST_PATH") purpose=registry-data ;;' \
  '  "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH") purpose=directory-data ;;' \
  '  "$PIGEONPOST_LOFT_DATA_HOST_PATH") purpose=loft-data ;;' \
  '  "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH") purpose=loft ;;' \
  '  "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH") purpose=registry-network ;;' \
  '  "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH") purpose=registry-identity ;;' \
  '  "$PIGEONPOST_PREFLIGHT_SECRET_FILE") purpose=secret ;;' \
  '  *) exit 64 ;;' \
  'esac' \
  'case "$purpose:$format" in' \
  '  secret:%s) wc -c < "$path" | tr -d " " ;;' \
  '  secret:%a) printf "%s\n" "${PIGEONPOST_PREFLIGHT_SECRET_MODE:-600}" ;;' \
  '  secret:%u) printf "%s\n" "${PIGEONPOST_PREFLIGHT_SECRET_OWNER:-10001}" ;;' \
  '  secret:%h) printf "%s\n" "${PIGEONPOST_PREFLIGHT_SECRET_LINKS:-1}" ;;' \
  '  registry-data:%d:%i) printf "%s\n" 1:100 ;;' \
  '  directory-data:%d:%i) printf "%s\n" 1:101 ;;' \
  '  loft-data:%d:%i) printf "%s\n" 1:102 ;;' \
  '  loft:%d:%i) printf "%s\n" 1:103 ;;' \
  '  registry-network:%d:%i) printf "%s\n" 1:104 ;;' \
  '  registry-identity:%d:%i) printf "%s\n" 1:105 ;;' \
  '  *:%a) printf "%s\n" "${PIGEONPOST_PREFLIGHT_PRIVATE_MODE:-700}" ;;' \
  '  *:%u) printf "%s\n" "${PIGEONPOST_PREFLIGHT_PRIVATE_OWNER:-10001}" ;;' \
  '  *) exit 64 ;;' \
  'esac' \
  > "$run_root/bin/stat"
chmod 0700 "$run_root/bin/gh" "$run_root/bin/docker" "$run_root/bin/stat"

image='ghcr.io/bekirdag/pigeonpost@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
export PATH="$run_root/bin:$PATH"
export PIGEONPOST_PREFLIGHT_CALL_LOG="$call_log"
export PIGEONPOST_IMAGE="$image"
export PIGEONPOST_ORIGIN='pigeonpost.dev/registry'
mkdir -p \
  "$run_root/registry-data" \
  "$run_root/directory-data" \
  "$run_root/loft-data" \
  "$run_root/traces/loft-network/nested" \
  "$run_root/traces/registry-network" \
  "$run_root/traces/registry-identity"
chmod 0700 \
  "$run_root/registry-data" \
  "$run_root/directory-data" \
  "$run_root/loft-data" \
  "$run_root/traces/loft-network" \
  "$run_root/traces/loft-network/nested" \
  "$run_root/traces/registry-network" \
  "$run_root/traces/registry-identity"
export PIGEONPOST_REGISTRY_DATA_HOST_PATH="$run_root/registry-data"
export PIGEONPOST_DIRECTORY_DATA_HOST_PATH="$run_root/directory-data"
export PIGEONPOST_LOFT_DATA_HOST_PATH="$run_root/loft-data"
export PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH="$run_root/traces/loft-network"
export PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH="$run_root/traces/registry-network"
export PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH="$run_root/traces/registry-identity"
secret_canary='provider-secret-must-never-reach-a-child-91fa'
secret_file="$run_root/github-client-secret"
printf '%s' "$secret_canary" > "$secret_file"
chmod 0600 "$secret_file"
export PIGEONPOST_GITHUB_CLIENT_ID='github-public-client-id'
export PIGEONPOST_GITHUB_CLIENT_SECRET_FILE="$secret_file"
export PIGEONPOST_GOOGLE_CLIENT_ID='google-public-client-id.apps.googleusercontent.com'
export PIGEONPOST_PREFLIGHT_EXPECTED_GITHUB_ID="$PIGEONPOST_GITHUB_CLIENT_ID"
export PIGEONPOST_PREFLIGHT_SECRET_FILE="$secret_file"
export PIGEONPOST_PREFLIGHT_EXPECTED_GOOGLE_ID="$PIGEONPOST_GOOGLE_CLIENT_ID"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_REGISTRY_DATA_HOST_PATH='' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/missing-registry-data-path.out" 2>&1; then
  echo 'preflight acceptance: missing registry data path was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_DIRECTORY_DATA_HOST_PATH='' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/missing-directory-data-path.out" 2>&1; then
  echo 'preflight acceptance: missing directory data path was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_LOFT_DATA_HOST_PATH='' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/missing-loft-data-path.out" 2>&1; then
  echo 'preflight acceptance: missing loft data path was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH='' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/missing-trace-path.out" 2>&1; then
  echo 'preflight acceptance: missing loft trace path was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' \
    PIGEONPOST_DIRECTORY_DATA_HOST_PATH="$PIGEONPOST_LOFT_DATA_HOST_PATH" \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/aliased-role-data.out" 2>&1; then
  echo 'preflight acceptance: directory data aliased to loft data was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_PREFLIGHT_PRIVATE_MODE=755 \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/public-private-directory.out" 2>&1; then
  echo 'preflight acceptance: publicly accessible private-storage directory was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' \
    PIGEONPOST_REGISTRY_DATA_HOST_PATH="$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH" \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/aliased-registry-data.out" 2>&1; then
  echo 'preflight acceptance: registry data aliased to a trace purpose was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' \
    PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH="$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH" \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/aliased-trace-directory.out" 2>&1; then
  echo 'preflight acceptance: aliased trace-purpose directories were accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' \
    PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH="$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH/nested" \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/nested-trace-directory.out" 2>&1; then
  echo 'preflight acceptance: nested trace-purpose directories were accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_GITHUB_CLIENT_SECRET="$secret_canary" \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/direct-secret.out" 2>&1; then
  echo 'preflight acceptance: direct provider secret environment was accepted' >&2
  exit 1
fi
test ! -e "$call_log"
if grep -Fq "$secret_canary" "$run_root/direct-secret.out"; then
  echo 'preflight acceptance: rejected direct secret reached output' >&2
  exit 1
fi

if PIGEONPOST_ALLOW_MOCK_IDENTITIES=1 \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/retired-mock.out" 2>&1; then
  echo 'preflight acceptance: retired mock identity mode was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES=1 \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/test-mock.out" 2>&1; then
  echo 'preflight acceptance: source-test mock identity mode was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_GITHUB_CLIENT_ID='' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/partial-provider.out" 2>&1; then
  echo 'preflight acceptance: secret file without a GitHub client ID was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_PREFLIGHT_SECRET_MODE=644 \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/public-secret.out" 2>&1; then
  echo 'preflight acceptance: publicly readable provider secret was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_PREFLIGHT_SECRET_OWNER=10002 \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/wrong-secret-owner.out" 2>&1; then
  echo 'preflight acceptance: provider secret owned by another account was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_PREFLIGHT_SECRET_LINKS=2 \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/multiply-linked-secret.out" 2>&1; then
  echo 'preflight acceptance: multiply-linked provider secret was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

: > "$secret_file"
if PIGEONPOST_RELEASE='v0.2.0' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/empty-secret.out" 2>&1; then
  echo 'preflight acceptance: empty provider secret was accepted' >&2
  exit 1
fi
test ! -e "$call_log"
printf '%s' "$secret_canary" > "$secret_file"

dd if=/dev/zero of="$secret_file" bs=4097 count=1 2>/dev/null
if PIGEONPOST_RELEASE='v0.2.0' \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/oversized-secret.out" 2>&1; then
  echo 'preflight acceptance: oversized provider secret was accepted' >&2
  exit 1
fi
test ! -e "$call_log"
printf '%s' "$secret_canary" > "$secret_file"

linked_secret="$run_root/linked-secret"
ln -s "$secret_file" "$linked_secret"
if PIGEONPOST_RELEASE='v0.2.0' PIGEONPOST_GITHUB_CLIENT_SECRET_FILE="$linked_secret" \
    "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/linked-secret.out" 2>&1; then
  echo 'preflight acceptance: linked provider secret was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

if PIGEONPOST_RELEASE='not-a-release' "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/invalid.out" 2>&1; then
  echo 'preflight acceptance: malformed release tag was accepted' >&2
  exit 1
fi
test ! -e "$call_log"

export PIGEONPOST_RELEASE='v0.2.0'
export PIGEONPOST_PREFLIGHT_IMMUTABLE=false
if "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/mutable.out" 2>&1; then
  echo 'preflight acceptance: mutable release was accepted' >&2
  exit 1
fi
if grep -Fq 'attestation verify' "$call_log" || \
   grep -Fq 'buildx imagetools inspect' "$call_log"; then
  echo 'preflight acceptance: mutable release reached artifact or image verification' >&2
  exit 1
fi

: > "$call_log"
export PIGEONPOST_PREFLIGHT_IMMUTABLE=true
export PIGEONPOST_PREFLIGHT_RELEASE_VERIFY=fail
if "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/unverified-release.out" 2>&1; then
  echo 'preflight acceptance: unattested release was accepted' >&2
  exit 1
fi
if grep -Fq 'attestation verify' "$call_log" || \
   grep -Fq 'buildx imagetools inspect' "$call_log"; then
  echo 'preflight acceptance: unattested release reached artifact or image verification' >&2
  exit 1
fi

: > "$call_log"
export PIGEONPOST_PREFLIGHT_RELEASE_VERIFY=pass
export PIGEONPOST_PREFLIGHT_RELEASE_IMAGE='ghcr.io/bekirdag/pigeonpost@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
if "$preflight" "$repo_root/deploy/compose.prod.yml" \
    >"$run_root/wrong-pointer.out" 2>&1; then
  echo 'preflight acceptance: image outside the immutable release was accepted' >&2
  exit 1
fi
if grep -Fq 'attestation verify oci://' "$call_log" || \
   grep -Fq 'buildx imagetools inspect' "$call_log"; then
  echo 'preflight acceptance: wrong release pointer reached image verification' >&2
  exit 1
fi

: > "$call_log"
unset PIGEONPOST_PREFLIGHT_RELEASE_IMAGE
"$preflight" "$repo_root/deploy/compose.prod.yml" >"$run_root/valid.out" 2>&1
grep -Fq 'release view v0.2.0 --repo bekirdag/pigeonpost --json isDraft,isImmutable,tagName' "$call_log"
grep -Fq 'release verify v0.2.0 --repo bekirdag/pigeonpost' "$call_log"
grep -Fq 'release download v0.2.0 --repo bekirdag/pigeonpost --pattern pigeonpost-container.txt --pattern SHA256SUMS' "$call_log"
grep -Fq 'attestation verify /' "$call_log"
grep -Fq "attestation verify oci://$image --bundle-from-oci" "$call_log"
grep -Fq -- '--signer-workflow github.com/bekirdag/pigeonpost/.github/workflows/release.yml' "$call_log"
grep -Fq -- '--source-ref refs/tags/v0.2.0' "$call_log"
grep -Fq -- '--source-digest aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$call_log"
grep -Fq -- '--deny-self-hosted-runners' "$call_log"
grep -Fq "buildx imagetools inspect $image" "$call_log"
grep -Fq 'compose -f' "$call_log"
grep -Fq 'immutable release, private-storage custody, exact image pointer/provenance' "$run_root/valid.out"
grep -Fq 'host quota enforcement is not proved' "$run_root/valid.out"
if grep -Fq "$secret_canary" "$call_log" "$run_root"/*.out; then
  echo 'preflight acceptance: provider secret appeared in subprocess arguments or output' >&2
  exit 1
fi

echo 'preflight provenance acceptance passed'
