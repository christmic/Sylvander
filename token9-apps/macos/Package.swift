// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Token9",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "Token9",
            path: "Sources/Token9"
        )
    ]
)
