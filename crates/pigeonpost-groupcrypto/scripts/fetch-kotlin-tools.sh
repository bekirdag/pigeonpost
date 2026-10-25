#!/usr/bin/env bash
# Download the pinned Kotlin compiler and JNA jar used by the Kotlin/JVM binding test, verifying
# SHA-256 digests. Prints the KOTLINC_HOME and JNA_JAR exports to use.
#   kotlin_tools_env="$(crates/pigeonpost-groupcrypto/scripts/fetch-kotlin-tools.sh <dir>)"
#   eval "$kotlin_tools_env"
set -euo pipefail
fetch() {
  local source_url="$1" output_path="$2" expected_sha="$3" temporary
  if [ ! -f "$output_path" ]; then
    temporary="$(mktemp "$output_path.download-XXXXXX")"
    # curl retries transient failures (including429); never publish a partial/unverified cache.
    if curl -fsSL --retry 4 --retry-delay 2 --retry-max-time 120 --connect-timeout 15 --max-time 60 \
        -o "$temporary" -- "$source_url"; then :; else return $?; fi
    if printf '%s  %s\n' "$expected_sha" "$temporary" | shasum -a 256 -c - >&2; then
      mv "$temporary" "$output_path"
    else
      return 1
    fi
  fi
  printf '%s  %s\n' "$expected_sha" "$output_path" | shasum -a 256 -c - >&2
}

main() {
  local tools_dir="${1:?target directory}"
  mkdir -p "$tools_dir" || return
  # Kotlin 2.2.21 matches apps/android; JNA 5.17.0 from Maven Central.
  fetch https://github.com/JetBrains/kotlin/releases/download/v2.2.21/kotlin-compiler-2.2.21.zip \
    "$tools_dir/kotlin-compiler-2.2.21.zip" a623871f1cd9c938946948b70ef9170879f0758043885bbd30c32f024e511714 || return
  fetch https://repo1.maven.org/maven2/net/java/dev/jna/jna/5.17.0/jna-5.17.0.jar \
    "$tools_dir/jna-5.17.0.jar" b3a9408e7c51e08ef0e3bfcc08f443f6ec0f6191ba8cd7c18d53d2b22e5bdbc0 || return
  [ -d "$tools_dir/kotlinc" ] || (cd "$tools_dir" && unzip -q kotlin-compiler-2.2.21.zip) || return
  printf 'export KOTLINC_HOME=%q JNA_JAR=%q\n' "$tools_dir/kotlinc" "$tools_dir/jna-5.17.0.jar"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
