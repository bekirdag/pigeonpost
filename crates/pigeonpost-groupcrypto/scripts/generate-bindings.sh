#!/usr/bin/env bash
# Regenerate the committed UniFFI bindings (Swift, Kotlin, Python, C#) from the FFI library.
#   cargo install uniffi-bindgen-cs --git https://github.com/NordSecurity/uniffi-bindgen-cs \
#     --tag v0.11.1+v0.31.0 --locked
#   crates/pigeonpost-groupcrypto/scripts/generate-bindings.sh [--check]
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"
cargo build --locked --release -p pigeonpost-groupcrypto-ffi
case "$(uname -s)" in
  Darwin) lib="target/release/libpigeonpost_groupcrypto_ffi.dylib" ;;
  MINGW*|MSYS*|CYGWIN*) lib="target/release/pigeonpost_groupcrypto_ffi.dll" ;;
  *) lib="target/release/libpigeonpost_groupcrypto_ffi.so" ;;
esac
out="bindings"
if [ "${1:-}" = "--check" ]; then out="$(mktemp -d)"; fi
for language in swift kotlin python; do
  cargo run --locked --release -q -p pigeonpost-groupcrypto-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --no-format --library "$lib" --language "$language" --out-dir "$out/$language"
done
uniffi-bindgen-cs --library --no-format --config ffi/uniffi.toml --out-dir "$out/csharp" "$lib"
if [ "${1:-}" = "--check" ]; then
  diff -r "$out" bindings && echo "committed bindings match the library"
fi
