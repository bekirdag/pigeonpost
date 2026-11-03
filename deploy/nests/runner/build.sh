#!/bin/sh
# Build the Nest runner image on the Nests host and print its digest-pinned reference.
#
#   build.sh <source-dir> [tag]
#
# <source-dir> is a checkout of this repository at the commit to ship (a clean git checkout of a
# merged commit in production). The build context is limited to what the two binaries need.
set -eu
SRC="$1"
TAG="${2:-localhost/pigeonpost-nest-runner:$(git -C "$SRC" rev-parse --short=12 HEAD 2>/dev/null || date -u +%Y%m%d%H%M%S)}"
CTX="$(mktemp -d)"
trap 'rm -rf "$CTX"' EXIT
cp "$SRC/Cargo.toml" "$SRC/Cargo.lock" "$SRC/rust-toolchain.toml" "$CTX/"
cp -R "$SRC/crates" "$SRC/openapi" "$CTX/"
mkdir -p "$CTX/deploy" && cp "$SRC/deploy/account-deletions.py" "$CTX/deploy/"
rm -rf "$CTX"/crates/*/target "$CTX"/crates/pigeonpost-groupcrypto/wasm/pkg 2>/dev/null || true
podman build --pull=never --runtime crun -f "$SRC/deploy/nests/runner/Containerfile" -t "$TAG" "$CTX" >&2
# The host service binary comes from the same pinned toolchain stage (cached by the build above).
podman build --pull=never --runtime crun --target build -f "$SRC/deploy/nests/runner/Containerfile" \
  -t localhost/pigeonpost-nests-build:latest "$CTX" >/dev/null
C="$(podman create localhost/pigeonpost-nests-build:latest)"
podman cp "$C:/src/target/release/pigeonpost-nestd" "${NESTD_OUT:-./pigeonpost-nestd}" >&2
podman rm "$C" >/dev/null
DIGEST="$(podman image inspect --format '{{.Digest}}' "$TAG")"
echo "${TAG%%:*}@${DIGEST}"
