// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "CovalentApple",
    platforms: [
        .macOS(.v15),
        .iOS(.v18),
    ],
    products: [
        .library(name: "CovalentShared", targets: ["CovalentShared"]),
    ],
    targets: [
        .target(
            name: "CovalentShared",
            path: "Sources/CovalentShared"
        ),
        .testTarget(
            name: "CovalentSharedTests",
            dependencies: ["CovalentShared"],
            path: "Tests/CovalentSharedTests"
        ),
    ]
)
