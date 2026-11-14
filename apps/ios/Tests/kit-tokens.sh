# Sourced by the test scripts that compile Shared/Design/Theme.swift with swiftc rather than Xcode.
# Theme reads its colours, radii and spacing from the design kit's generated tokens (`PP.*` in
# Packages/PigeonpostUI). Those two files are built here as a small `PigeonpostUI` module, so
# `import PigeonpostUI` resolves the same way it does in the app, without the kit's SwiftUI
# components or its font bundle. Expects $out (the script's temporary directory); sets $kit to the
# flags a swiftc command needs to import and link it.
kit_src=Packages/PigeonpostUI/Sources/PigeonpostUI/Generated
mkdir -p "$out/kit"
swiftc -O -parse-as-library -module-name PigeonpostUI -emit-module -emit-module-path "$out/kit/PigeonpostUI.swiftmodule" \
  -emit-library -static -o "$out/kit/libPigeonpostUI.a" "$kit_src/PPTokens.swift" "$kit_src/PPScales.swift"
kit="-I $out/kit -L $out/kit -lPigeonpostUI"
