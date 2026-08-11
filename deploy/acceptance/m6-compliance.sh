#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)

require_executable() {
  local name=$1
  local value=${!name:-}
  if [[ -z "$value" || "$value" != /* || ! -f "$value" || ! -x "$value" ]]; then
    echo "M6 acceptance: $name must name an absolute executable regular file" >&2
    exit 1
  fi
}

require_executable PIGEONPOST_BIN
require_executable PPCOMPLIANCE_BIN
require_executable PIGEONPOST_M6_ADAPTER_BIN

cd "$repo_root"
cargo test --locked -p pigeonpost-compliance \
  --test m6_binary_acceptance \
  m6_exact_binaries_complete_the_compliance_lifecycle \
  -- --ignored --exact --nocapture

echo "M6 acceptance: exact online/offline binary lifecycle passed"
