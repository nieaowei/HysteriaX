// swift-tools-version: 6.4
import PackageDescription

let package = Package(
    name: "HysteriaX",
    platforms: [.macOS(.v27)],
    products: [.executable(name: "HysteriaX", targets: ["HysteriaX"])],
    targets: [
        .executableTarget(name: "HysteriaX", path: "Sources/HysteriaX")
    ]
)
