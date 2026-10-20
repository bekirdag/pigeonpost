#!/usr/bin/env bash
# Build PigeonpostGroupCryptoFFI.xcframework (iOS device, iOS simulator arm64+x86_64, macOS
# arm64+x86_64) and stage the Swift package in apple/. Requires macOS with Xcode.
#   crates/pigeonpost-groupcrypto/scripts/build-apple.sh
# Then: (cd crates/pigeonpost-groupcrypto/apple && swift test)
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"
export IPHONEOS_DEPLOYMENT_TARGET=17.0 MACOSX_DEPLOYMENT_TARGET=14.0
targets=(aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${targets[@]}" >/dev/null
for t in "${targets[@]}"; do
  cargo build --locked --release -p pigeonpost-groupcrypto-ffi --target "$t"
done
work="target/apple"
rm -rf "$work" apple/PigeonpostGroupCryptoFFI.xcframework
mkdir -p "$work/headers" "$work/ios-sim" "$work/macos"
cp bindings/swift/PigeonpostGroupCryptoFFI.h "$work/headers/"
cp bindings/swift/PigeonpostGroupCryptoFFI.modulemap "$work/headers/module.modulemap"
lib=libpigeonpost_groupcrypto_ffi.a
lipo -create "target/aarch64-apple-ios-sim/release/$lib" "target/x86_64-apple-ios/release/$lib" -output "$work/ios-sim/$lib"
lipo -create "target/aarch64-apple-darwin/release/$lib" "target/x86_64-apple-darwin/release/$lib" -output "$work/macos/$lib"
xcodebuild -create-xcframework \
  -library "target/aarch64-apple-ios/release/$lib" -headers "$work/headers" \
  -library "$work/ios-sim/$lib" -headers "$work/headers" \
  -library "$work/macos/$lib" -headers "$work/headers" \
  -output apple/PigeonpostGroupCryptoFFI.xcframework
mkdir -p apple/Sources/PigeonpostGroupCrypto
cp bindings/swift/PigeonpostGroupCrypto.swift apple/Sources/PigeonpostGroupCrypto/
echo "built apple/PigeonpostGroupCryptoFFI.xcframework"
