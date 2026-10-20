#!/usr/bin/env bash
# Build the Android AAR: JNI libraries for arm64-v8a, armeabi-v7a and x86_64 (API 26, the app's
# minSdk, 16 KB page aligned) plus the generated Kotlin binding compiled to classes.jar. Consumers also depend on
# net.java.dev.jna:jna:5.17.0@aar, which provides libjnidispatch for each ABI.
#   ANDROID_NDK_HOME=... KOTLINC_HOME=... JNA_JAR=... crates/pigeonpost-groupcrypto/scripts/build-android.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"
ndk="${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
api=26
case "$(uname -s)" in Darwin) host=darwin-x86_64 ;; *) host=linux-x86_64 ;; esac
bin="$ndk/toolchains/llvm/prebuilt/$host/bin"
out="target/android"
rm -rf "$out" && mkdir -p "$out/aar/jni" "$out/classes"
build() {
  local triple="$1" abi="$2" clang="$3"
  local upper; upper="$(echo "$triple" | tr 'a-z-' 'A-Z_')"
  rustup target add "$triple" >/dev/null
  env "CC_${triple//-/_}=$bin/$clang" "AR_${triple//-/_}=$bin/llvm-ar" \
      "CARGO_TARGET_${upper}_LINKER=$bin/$clang" \
      "CARGO_TARGET_${upper}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384" \
      cargo build --locked --release -p pigeonpost-groupcrypto-ffi --target "$triple"
  mkdir -p "$out/aar/jni/$abi"
  cp "target/$triple/release/libpigeonpost_groupcrypto_ffi.so" "$out/aar/jni/$abi/"
  "$bin/llvm-strip" --strip-unneeded "$out/aar/jni/$abi/libpigeonpost_groupcrypto_ffi.so"
  # Google Play requires 16 KB page alignment for 64-bit libraries.
  if [ "$abi" != armeabi-v7a ]; then
    for align in $("$bin/llvm-readelf" -lW "$out/aar/jni/$abi/libpigeonpost_groupcrypto_ffi.so" | awk '$1 == "LOAD" { print $NF }'); do
      if (( align < 16384 )); then echo "$abi library is not 16 KB aligned ($align)" >&2; exit 1; fi
    done
  fi
}
build aarch64-linux-android arm64-v8a "aarch64-linux-android${api}-clang"
build armv7-linux-androideabi armeabi-v7a "armv7a-linux-androideabi${api}-clang"
build x86_64-linux-android x86_64 "x86_64-linux-android${api}-clang"
"${KOTLINC_HOME:?set KOTLINC_HOME}/bin/kotlinc" -nowarn -jvm-target 11 -cp "${JNA_JAR:?set JNA_JAR}" \
  bindings/kotlin/dev/pigeonpost/groupcrypto/pigeonpost_groupcrypto.kt -d "$out/classes"
(cd "$out/classes" && rm -rf META-INF/main.kotlin_module && jar cf ../aar/classes.jar .)
cat > "$out/aar/AndroidManifest.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="dev.pigeonpost.groupcrypto">
    <uses-sdk android:minSdkVersion="$api" />
</manifest>
XML
: > "$out/aar/R.txt"
(cd "$out/aar" && zip -qr ../pigeonpost-groupcrypto.aar .)
ls -l "$out"/aar/jni/*/*.so "$out/pigeonpost-groupcrypto.aar"
