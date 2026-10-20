#!/usr/bin/env bash
# Build every device agent that this host can run and execute the cross-client scenario
# (interop/run_interop.py). Required: Rust, Python 3, Node >= 20. Optional (used when present):
#   swiftc (macOS)                         -> Swift binding agent
#   java + KOTLINC_HOME + JNA_JAR          -> Kotlin/JVM binding agent (see fetch-kotlin-tools.sh)
#   dotnet (8+)                            -> C# binding agent
# PYTHON overrides the interpreter (default python3).
# Set GROUPCRYPTO_REQUIRE="swift kotlin csharp" to fail instead of skipping missing toolchains.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
# Native Windows processes (Python, node, .NET) need D:/... paths, not Git Bash's /d/...
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) here="$(cd "$here" && pwd -W)" ;; esac
cd "$here"
python="${PYTHON:-python3}"
out="$here/target/interop"
mkdir -p "$out"
require="${GROUPCRYPTO_REQUIRE:-}"
need() { case " $require " in *" $1 "*) echo "required agent '$1' cannot be built: $2" >&2; exit 1 ;; esac; echo "skipping $1: $2"; }

cargo build --locked --release -p pigeonpost-groupcrypto-interop-device -p pigeonpost-groupcrypto-ffi
case "$(uname -s)" in
  Darwin) lib="libpigeonpost_groupcrypto_ffi.dylib" ;;
  MINGW*|MSYS*|CYGWIN*) lib="pigeonpost_groupcrypto_ffi.dll" ;;
  *) lib="libpigeonpost_groupcrypto_ffi.so" ;;
esac
rust="$here/target/release/rust-device"
agents=(--agent "rust=$rust" --agent "wasm=node $here/interop/node-device.mjs")

mkdir -p "$out/python"
cp bindings/python/pigeonpost_groupcrypto.py "target/release/$lib" "$out/python/"
agents+=(--agent "python=$python -I $here/interop/python_device.py $out/python")

if command -v swiftc >/dev/null 2>&1 && [ "$(uname -s)" = "Darwin" ]; then
  swiftc -O -I bindings/swift -Xcc -fmodule-map-file=bindings/swift/PigeonpostGroupCryptoFFI.modulemap \
    bindings/swift/PigeonpostGroupCrypto.swift interop/swift-device/main.swift \
    target/release/libpigeonpost_groupcrypto_ffi.a -o "$out/swift-device"
  agents+=(--agent "swift=$out/swift-device")
else
  need swift "swiftc on macOS not available"
fi

cpsep=":"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) cpsep=";" ;; esac
if command -v java >/dev/null 2>&1 && [ -n "${KOTLINC_HOME:-}" ] && [ -f "${JNA_JAR:-}" ]; then
  "$KOTLINC_HOME/bin/kotlinc" -nowarn -cp "$JNA_JAR" bindings/kotlin/dev/pigeonpost/groupcrypto/pigeonpost_groupcrypto.kt \
    interop/kotlin-device/Main.kt -d "$out/kotlin-device.jar"
  agents+=(--agent "kotlin=java -Djna.library.path=$here/target/release -cp $out/kotlin-device.jar$cpsep$JNA_JAR$cpsep$KOTLINC_HOME/lib/kotlin-stdlib.jar MainKt")
else
  need kotlin "set KOTLINC_HOME and JNA_JAR (scripts/fetch-kotlin-tools.sh)"
fi

if command -v dotnet >/dev/null 2>&1; then
  dotnet build interop/csharp-device -c Release -o "$out/csharp-device" -nologo -v quiet
  cp "target/release/$lib" "$out/csharp-device/"
  agents+=(--agent "csharp=$out/csharp-device/csharp-device")
else
  need csharp "dotnet not available"
fi

# Run twice, the second time with the agent order reversed, so each binding plays different roles
# (creator, remover, adder, concurrent-commit winner and loser, offline device, removed device).
"$python" interop/run_interop.py --validator "$rust" --extra "wasm=node $here/interop/node-device.mjs" "${agents[@]}"
reversed=()
for ((i=${#agents[@]}-1; i>=1; i-=2)); do reversed+=("${agents[i-1]}" "${agents[i]}"); done
"$python" interop/run_interop.py --validator "$rust" --extra "python=$python -I $here/interop/python_device.py $out/python" "${reversed[@]}"
