// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "VisionBridge",
    platforms: [
        .macOS(.v11)
    ],
    products: [
        .library(
            name: "VisionBridge",
            type: .dynamic,
            targets: ["VisionBridge"]
        ),
    ],
    targets: [
        .target(
            name: "VisionBridge",
            dependencies: [],
            path: "Sources/VisionBridge",
            publicHeadersPath: "."
        ),
    ]
)
