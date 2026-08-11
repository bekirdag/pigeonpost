#!/bin/sh
set -eu

# Capture only public identifiers and the secret-file location, then remove every provider setting
# from the inherited environment before the first subprocess. A direct secret value is rejected
# before even `dirname`, `grep`, `gh`, checksum tools, or Docker can inherit it.
github_secret_env_is_set=${PIGEONPOST_GITHUB_CLIENT_SECRET+x}
github_id=${PIGEONPOST_GITHUB_CLIENT_ID:-}
github_secret_file=${PIGEONPOST_GITHUB_CLIENT_SECRET_FILE:-}
google_id=${PIGEONPOST_GOOGLE_CLIENT_ID:-}
allow_mock=${PIGEONPOST_ALLOW_MOCK_IDENTITIES:-0}
allow_test_mock=${PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES:-0}
allow_insecure_secret_env=${PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV:-0}
if [ "$github_secret_env_is_set" = x ]; then
  printf '%s\n' 'preflight: direct provider-secret environment values are forbidden in production' >&2
  exit 1
fi
case "$allow_mock" in
  0|'') ;;
  1)
    printf '%s\n' 'preflight: mock identities are forbidden in production' >&2
    exit 1
    ;;
  *)
    printf '%s\n' 'preflight: PIGEONPOST_ALLOW_MOCK_IDENTITIES must be 0 or 1' >&2
    exit 1
    ;;
esac
case "$allow_test_mock" in
  0|'') ;;
  1)
    printf '%s\n' 'preflight: source-test mock identities are forbidden in production' >&2
    exit 1
    ;;
  *)
    printf '%s\n' 'preflight: PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES must be 0 or 1' >&2
    exit 1
    ;;
esac
case "$allow_insecure_secret_env" in
  0|'') ;;
  *)
    printf '%s\n' 'preflight: insecure provider-secret environment mode is forbidden in production' >&2
    exit 1
    ;;
esac
if { [ -n "$github_id" ] && [ -z "$github_secret_file" ]; } ||
   { [ -z "$github_id" ] && [ -n "$github_secret_file" ]; }; then
  printf '%s\n' 'preflight: GitHub identity configuration requires a client ID and secret file' >&2
  exit 1
fi
case "$github_secret_file" in
  ''|/*) ;;
  *)
    printf '%s\n' 'preflight: PIGEONPOST_GITHUB_CLIENT_SECRET_FILE must be absolute' >&2
    exit 1
    ;;
esac
unset PIGEONPOST_GITHUB_CLIENT_ID PIGEONPOST_GITHUB_CLIENT_SECRET
unset PIGEONPOST_GITHUB_CLIENT_SECRET_FILE PIGEONPOST_GOOGLE_CLIENT_ID
unset PIGEONPOST_ALLOW_MOCK_IDENTITIES PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES
unset PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
compose_file=${1:-"$script_dir/compose.prod.yml"}

: "${PIGEONPOST_IMAGE:?Set PIGEONPOST_IMAGE to the digest from pigeonpost-container.txt}"
: "${PIGEONPOST_ORIGIN:?Set PIGEONPOST_ORIGIN to the public registry origin}"
: "${PIGEONPOST_RELEASE:?Set PIGEONPOST_RELEASE to the verified immutable vX.Y.Z release}"
: "${PIGEONPOST_REGISTRY_DATA_HOST_PATH:?Set PIGEONPOST_REGISTRY_DATA_HOST_PATH to the preprovisioned quota-managed registry data directory}"
: "${PIGEONPOST_DIRECTORY_DATA_HOST_PATH:?Set PIGEONPOST_DIRECTORY_DATA_HOST_PATH to the preprovisioned quota-managed directory data directory}"
: "${PIGEONPOST_LOFT_DATA_HOST_PATH:?Set PIGEONPOST_LOFT_DATA_HOST_PATH to the preprovisioned quota-managed loft data directory}"
: "${PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH:?Set PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH to the preprovisioned loft network-trace directory}"
: "${PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH:?Set PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH to the preprovisioned registry network-trace directory}"
: "${PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH:?Set PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH to the preprovisioned registry identity-trace directory}"

if ! printf '%s\n' "$PIGEONPOST_RELEASE" \
    | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then
  echo "preflight: PIGEONPOST_RELEASE must be a stable vX.Y.Z tag" >&2
  exit 1
fi

prefix='ghcr.io/bekirdag/pigeonpost@sha256:'
case "$PIGEONPOST_IMAGE" in
  "$prefix"*) ;;
  *)
    echo "preflight: PIGEONPOST_IMAGE must be the official GHCR digest reference" >&2
    exit 1
    ;;
esac

digest=${PIGEONPOST_IMAGE#"$prefix"}
if [ "${#digest}" -ne 64 ]; then
  echo "preflight: image digest must contain exactly 64 hexadecimal characters" >&2
  exit 1
fi
case "$digest" in
  *[!0-9a-f]*)
    echo "preflight: image digest must be lowercase hexadecimal" >&2
    exit 1
    ;;
esac

command -v docker >/dev/null 2>&1 || {
  echo "preflight: docker is not installed" >&2
  exit 1
}
command -v gh >/dev/null 2>&1 || {
  echo "preflight: the GitHub CLI is required to verify image provenance" >&2
  exit 1
}
command -v sha256sum >/dev/null 2>&1 || {
  echo "preflight: sha256sum is required to verify the released image pointer" >&2
  exit 1
}
command -v stat >/dev/null 2>&1 || {
  echo "preflight: GNU stat is required to verify provider secret and private-storage custody" >&2
  exit 1
}

validate_private_directory() {
  label=$1
  path=$2
  case "$path" in
    /*) ;;
    *)
      echo "preflight: $label must be an absolute path" >&2
      exit 1
      ;;
  esac
  if [ ! -d "$path" ] || [ -L "$path" ]; then
    echo "preflight: $label must be a preprovisioned private directory, not a link" >&2
    exit 1
  fi
  canonical=$(CDPATH='' cd -P -- "$path" && pwd -P)
  if [ "$canonical" != "$path" ]; then
    echo "preflight: $label must be supplied as its canonical physical path" >&2
    exit 1
  fi
  private_owner=$(stat -c %u -- "$path")
  private_mode=$(stat -c %a -- "$path")
  if [ "$private_owner" != 10001 ]; then
    echo "preflight: $label must be owned by container uid 10001" >&2
    exit 1
  fi
  if [ "$private_mode" != 700 ]; then
    echo "preflight: $label mode must be 0700" >&2
    exit 1
  fi
}

validate_private_directory \
  PIGEONPOST_REGISTRY_DATA_HOST_PATH "$PIGEONPOST_REGISTRY_DATA_HOST_PATH"
validate_private_directory \
  PIGEONPOST_DIRECTORY_DATA_HOST_PATH "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH"
validate_private_directory \
  PIGEONPOST_LOFT_DATA_HOST_PATH "$PIGEONPOST_LOFT_DATA_HOST_PATH"
validate_private_directory \
  PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH"
validate_private_directory \
  PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH"
validate_private_directory \
  PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"

validate_separate_private_directories() {
  first_label=$1
  first=$2
  second_label=$3
  second=$4
  first_identity=$(stat -c '%d:%i' -- "$first")
  second_identity=$(stat -c '%d:%i' -- "$second")
  if [ "$first_identity" = "$second_identity" ]; then
    echo "preflight: $first_label and $second_label must be distinct directories" >&2
    exit 1
  fi
  case "$first/" in
    "$second/"*)
      echo "preflight: $first_label and $second_label must not be nested" >&2
      exit 1
      ;;
  esac
  case "$second/" in
    "$first/"*)
      echo "preflight: $first_label and $second_label must not be nested" >&2
      exit 1
      ;;
  esac
}

validate_separate_private_directories \
  PIGEONPOST_REGISTRY_DATA_HOST_PATH "$PIGEONPOST_REGISTRY_DATA_HOST_PATH" \
  PIGEONPOST_DIRECTORY_DATA_HOST_PATH "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_REGISTRY_DATA_HOST_PATH "$PIGEONPOST_REGISTRY_DATA_HOST_PATH" \
  PIGEONPOST_LOFT_DATA_HOST_PATH "$PIGEONPOST_LOFT_DATA_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_DIRECTORY_DATA_HOST_PATH "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH" \
  PIGEONPOST_LOFT_DATA_HOST_PATH "$PIGEONPOST_LOFT_DATA_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_REGISTRY_DATA_HOST_PATH "$PIGEONPOST_REGISTRY_DATA_HOST_PATH" \
  PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_REGISTRY_DATA_HOST_PATH "$PIGEONPOST_REGISTRY_DATA_HOST_PATH" \
  PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_REGISTRY_DATA_HOST_PATH "$PIGEONPOST_REGISTRY_DATA_HOST_PATH" \
  PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_DIRECTORY_DATA_HOST_PATH "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH" \
  PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_DIRECTORY_DATA_HOST_PATH "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH" \
  PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_DIRECTORY_DATA_HOST_PATH "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH" \
  PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_LOFT_DATA_HOST_PATH "$PIGEONPOST_LOFT_DATA_HOST_PATH" \
  PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_LOFT_DATA_HOST_PATH "$PIGEONPOST_LOFT_DATA_HOST_PATH" \
  PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_LOFT_DATA_HOST_PATH "$PIGEONPOST_LOFT_DATA_HOST_PATH" \
  PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH \
  "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH" \
  PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH \
  "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH \
  "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH" \
  PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH \
  "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"
validate_separate_private_directories \
  PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH \
  "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH" \
  PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH \
  "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"

if [ -n "$github_secret_file" ]; then
  if [ ! -f "$github_secret_file" ] || [ -L "$github_secret_file" ]; then
    echo "preflight: GitHub client secret must be a regular file, not a link" >&2
    exit 1
  fi
  secret_size=$(stat -c %s -- "$github_secret_file")
  secret_mode=$(stat -c %a -- "$github_secret_file")
  secret_owner=$(stat -c %u -- "$github_secret_file")
  secret_links=$(stat -c %h -- "$github_secret_file")
  case "$secret_size" in
    ''|*[!0-9]*)
      echo "preflight: could not determine GitHub client-secret size" >&2
      exit 1
      ;;
  esac
  if [ "$secret_size" -eq 0 ] || [ "$secret_size" -gt 4096 ]; then
    echo "preflight: GitHub client secret must be nonempty and no larger than 4 KiB" >&2
    exit 1
  fi
  case "$secret_mode" in
    400|600) ;;
    *)
      echo "preflight: GitHub client-secret file mode must be 0400 or 0600" >&2
      exit 1
      ;;
  esac
  if [ "$secret_owner" != 10001 ]; then
    echo "preflight: GitHub client-secret file must be owned by container uid 10001" >&2
    exit 1
  fi
  if [ "$secret_links" != 1 ]; then
    echo "preflight: GitHub client-secret file must have exactly one filesystem link" >&2
    exit 1
  fi
fi

release_state=$(gh release view "$PIGEONPOST_RELEASE" \
  --repo bekirdag/pigeonpost \
  --json isDraft,isImmutable,tagName \
  --jq '[.isDraft,.isImmutable,.tagName] | @tsv')
tab=$(printf '\t')
if [ "$release_state" != "false${tab}true${tab}$PIGEONPOST_RELEASE" ]; then
  echo "preflight: release must exist, be published, and be immutable" >&2
  exit 1
fi
gh release verify "$PIGEONPOST_RELEASE" \
  --repo bekirdag/pigeonpost >/dev/null

source_sha=$(gh api -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/bekirdag/pigeonpost/commits/$PIGEONPOST_RELEASE" --jq .sha)
if ! printf '%s\n' "$source_sha" | grep -Eq '^[0-9a-f]{40}$'; then
  echo "preflight: release tag did not resolve to one commit" >&2
  exit 1
fi

release_dir=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-preflight-release.XXXXXX")
cleanup_release_dir() {
  rm -rf -- "$release_dir"
}
trap cleanup_release_dir EXIT HUP INT TERM
gh release download "$PIGEONPOST_RELEASE" \
  --repo bekirdag/pigeonpost \
  --pattern pigeonpost-container.txt \
  --pattern SHA256SUMS \
  --dir "$release_dir"
pointer="$release_dir/pigeonpost-container.txt"
checksums="$release_dir/SHA256SUMS"
if [ ! -f "$pointer" ] || [ -L "$pointer" ] || \
   [ ! -f "$checksums" ] || [ -L "$checksums" ]; then
  echo "preflight: immutable release lacks regular image-pointer/checksum assets" >&2
  exit 1
fi
if [ "$(grep -Ec '^[0-9a-f]{64}  pigeonpost-container\.txt$' "$checksums")" -ne 1 ]; then
  echo "preflight: release checksum manifest does not name the image pointer exactly once" >&2
  exit 1
fi
expected_pointer_digest=$(grep -E '^[0-9a-f]{64}  pigeonpost-container\.txt$' "$checksums" \
  | cut -d' ' -f1)
actual_pointer_digest=$(sha256sum "$pointer" | cut -d' ' -f1)
if [ "$actual_pointer_digest" != "$expected_pointer_digest" ]; then
  echo "preflight: released image-pointer checksum does not match" >&2
  exit 1
fi
expected_pointer_bytes=$(printf '%s\n' "$PIGEONPOST_IMAGE" | sha256sum | cut -d' ' -f1)
if [ "$actual_pointer_digest" != "$expected_pointer_bytes" ]; then
  echo "preflight: PIGEONPOST_IMAGE is not the exact immutable release pointer" >&2
  exit 1
fi
gh attestation verify "$pointer" \
  --repo bekirdag/pigeonpost \
  --signer-workflow github.com/bekirdag/pigeonpost/.github/workflows/release.yml \
  --source-ref "refs/tags/$PIGEONPOST_RELEASE" \
  --source-digest "$source_sha" \
  --deny-self-hosted-runners >/dev/null
gh attestation verify "oci://$PIGEONPOST_IMAGE" \
  --bundle-from-oci \
  --repo bekirdag/pigeonpost \
  --signer-workflow github.com/bekirdag/pigeonpost/.github/workflows/release.yml \
  --source-ref "refs/tags/$PIGEONPOST_RELEASE" \
  --source-digest "$source_sha" \
  --deny-self-hosted-runners >/dev/null

docker compose version >/dev/null
docker buildx imagetools inspect "$PIGEONPOST_IMAGE" >/dev/null
PIGEONPOST_GITHUB_CLIENT_ID=$github_id \
PIGEONPOST_GITHUB_CLIENT_SECRET_FILE=$github_secret_file \
PIGEONPOST_GOOGLE_CLIENT_ID=$google_id \
  docker compose -f "$compose_file" config --quiet

echo "preflight: immutable release, private-storage custody, exact image pointer/provenance, and Compose configuration are valid"
echo "preflight: host quota enforcement is not proved; record quota and free-space evidence for every role-data and trace-purpose path"
echo "preflight: confirm checkpoint-key and volume backups before changing running services"
echo "preflight: confirm loft.toml/registry.toml, witness cache, trace keys, and custody prerequisites"
