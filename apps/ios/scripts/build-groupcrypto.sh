#!/bin/sh
# Build the group end-to-end encryption library (crates/pigeonpost-groupcrypto, Rust + UniFFI) as a
# static library for whatever Xcode is building right now, and leave it where the app's linker looks.
#
# Run by the "Group crypto (Rust)" build phase of both app targets, and by Tests/run.sh for the host
# Mac. Reproducible from the repository alone: the crate pins its toolchain (rust-toolchain.toml,
# Rust 1.91.1) and its dependencies (Cargo.lock, --locked); the Swift binding and the C header are
# committed under crates/pigeonpost-groupcrypto/bindings/swift and are compiled from there, so the
# app and the crate's own CI check (`scripts/generate-bindings.sh --check`) read the same files.
#
#   apps/ios/scripts/build-groupcrypto.sh [output-dir]
#
# Inputs (Xcode sets them): PLATFORM_NAME (iphoneos | iphonesimulator | macosx), ARCHS.
# Output: <output-dir>/libpigeonpost_groupcrypto_ffi.a (one fat library for the requested archs).
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/../../../crates/pigeonpost-groupcrypto" && pwd)"
platform="${PLATFORM_NAME:-macosx}"
archs="${ARCHS:-$(uname -m)}"
out="${1:-${TARGET_TEMP_DIR:-$crate/target/xcode}/groupcrypto}"
lib=libpigeonpost_groupcrypto_ffi.a

# Xcode runs this with its own environment: SDKROOT, the deployment targets and the compiler
# variables all point at the platform being built, and a build script (which runs on this Mac)
# would then try to link against the iPhone SDK. Cargo gets a clean environment instead.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
path="$cargo_home/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
if ! PATH="$path" command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust with rustup (https://rustup.rs); the crate pins its own toolchain." >&2
  exit 1
fi
toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$crate/rust-toolchain.toml")"

targets=""
for arch in $archs; do
  case "$platform/$arch" in
    iphoneos/arm64) t=aarch64-apple-ios ;;
    iphonesimulator/arm64) t=aarch64-apple-ios-sim ;;
    iphonesimulator/x86_64) t=x86_64-apple-ios ;;
    macosx/arm64) t=aarch64-apple-darwin ;;
    macosx/x86_64) t=x86_64-apple-darwin ;;
    *) echo "warning: no Rust target for $platform/$arch; skipped" >&2; continue ;;
  esac
  targets="$targets $t"
done
[ -n "$targets" ] || { echo "error: nothing to build for $platform ($archs)" >&2; exit 1; }

run() {
  env -i HOME="$HOME" PATH="$path" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" \
    ${DEVELOPER_DIR:+DEVELOPER_DIR="$DEVELOPER_DIR"} \
    IPHONEOS_DEPLOYMENT_TARGET=17.0 MACOSX_DEPLOYMENT_TARGET=14.0 "$@"
}

# shellcheck disable=SC2086
run rustup target add --toolchain "$toolchain" $targets >/dev/null
parts=""
for t in $targets; do
  (cd "$crate" && run cargo rustc --locked --release -p pigeonpost-groupcrypto-ffi --lib \
    --crate-type staticlib --target "$t")
  parts="$parts $crate/target/$t/release/$lib"
done

mkdir -p "$out"
# shellcheck disable=SC2086
lipo -create $parts -output "$out/$lib.new"
if [ -f "$out/$lib" ] && cmp -s "$out/$lib.new" "$out/$lib"; then
  rm "$out/$lib.new"   # unchanged: keep the timestamp so the app is not relinked for nothing
else
  mv "$out/$lib.new" "$out/$lib"
fi
echo "groupcrypto: $out/$lib ($platform:$targets)"
