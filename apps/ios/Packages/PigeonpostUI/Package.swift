// swift-tools-version:5.9
// Pigeonpost UI 1.0.0 — SwiftUI components for the Pigeonpost iOS, iPadOS and macOS apps.
import PackageDescription

let package = Package(
    name: "PigeonpostUI",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "PigeonpostUI", targets: ["PigeonpostUI"])
    ],
    targets: [
        .target(
            name: "PigeonpostUI",
            resources: [.copy("Resources/Fonts")]
        ),
        .testTarget(
            name: "PigeonpostUITests",
            dependencies: ["PigeonpostUI"]
        )
    ]
)
