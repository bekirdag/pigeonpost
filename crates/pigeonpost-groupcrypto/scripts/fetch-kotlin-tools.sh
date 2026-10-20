#!/usr/bin/env bash
# Download the pinned Kotlin compiler and JNA jar used by the Kotlin/JVM binding test, verifying
# SHA-256 digests. Prints the KOTLINC_HOME and JNA_JAR exports to use.
#   eval "$(crates/pigeonpost-groupcrypto/scripts/fetch-kotlin-tools.sh <dir>)"
set -euo pipefail
dir="${1:?target directory}"
mkdir -p "$dir"
fetch() {
  local url="$1" file="$2" sha="$3"
  [ -f "$dir/$file" ] || curl -fsSL -o "$dir/$file" "$url"
  echo "$sha  $dir/$file" | shasum -a 256 -c - >&2
}
# Kotlin 2.2.21 matches apps/android; JNA 5.17.0 from Maven Central.
fetch https://github.com/JetBrains/kotlin/releases/download/v2.2.21/kotlin-compiler-2.2.21.zip \
  kotlin-compiler-2.2.21.zip a623871f1cd9c938946948b70ef9170879f0758043885bbd30c32f024e511714
fetch https://repo1.maven.org/maven2/net/java/dev/jna/jna/5.17.0/jna-5.17.0.jar \
  jna-5.17.0.jar b3a9408e7c51e08ef0e3bfcc08f443f6ec0f6191ba8cd7c18d53d2b22e5bdbc0
[ -d "$dir/kotlinc" ] || (cd "$dir" && unzip -q kotlin-compiler-2.2.21.zip)
echo "export KOTLINC_HOME=$dir/kotlinc JNA_JAR=$dir/jna-5.17.0.jar"
