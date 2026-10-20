// swift-tools-version:5.9
// Swift package for the Pigeonpost group E2EE binding. `scripts/build-apple.sh` builds the
// XCFramework and copies the generated binding into Sources/ (both are build outputs, not
// committed). Add this package to the app as a local package dependency.
import PackageDescription

let package = Package(
    name: "PigeonpostGroupCrypto",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "PigeonpostGroupCrypto", targets: ["PigeonpostGroupCrypto"]),
    ],
    targets: [
        .binaryTarget(name: "PigeonpostGroupCryptoFFI", path: "PigeonpostGroupCryptoFFI.xcframework"),
        .target(name: "PigeonpostGroupCrypto", dependencies: ["PigeonpostGroupCryptoFFI"]),
        .testTarget(name: "PigeonpostGroupCryptoTests", dependencies: ["PigeonpostGroupCrypto"]),
    ]
)
