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
    dependencies: [
        .package(url: "https://github.com/weichsel/ZIPFoundation.git", exact: "0.9.20"),
    ],
    targets: [
        .target(
            name: "CovalentShared",
            dependencies: [
                .product(name: "ZIPFoundation", package: "ZIPFoundation"),
            ],
            path: "Sources/CovalentShared"
        ),
        .testTarget(
            name: "CovalentSharedTests",
            dependencies: ["CovalentShared"],
            path: "Tests/CovalentSharedTests"
        ),
    ]
)
