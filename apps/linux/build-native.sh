#!/usr/bin/env bash
# Build the group end-to-end encryption library (crates/pigeonpost-groupcrypto, UniFFI) for this
# machine and place it, with its generated Python binding, where the app loads it:
#   pigeonpost/native/<x86_64|aarch64>/{pigeonpost_groupcrypto.py, libpigeonpost_groupcrypto_ffi.so}
# Needs Rust (rustup picks 1.91.1 from the crate's rust-toolchain.toml). Release CI builds both
# processors on Ubuntu 22.04 so the library runs on Debian 12 and Ubuntu 22.04 or newer.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
crate="$here/../../crates/pigeonpost-groupcrypto"
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported processor: $(uname -m)" >&2; exit 1 ;;
esac
(cd "$crate" && cargo build --locked --release -p pigeonpost-groupcrypto-ffi)
target="${CARGO_TARGET_DIR:-$crate/target}"
dest="$here/pigeonpost/native/$arch"
mkdir -p "$dest"
cp "$crate/bindings/python/pigeonpost_groupcrypto.py" "$dest/"
install -m 0644 "$target/release/libpigeonpost_groupcrypto_ffi.so" "$dest/"
echo "$dest"
