// swift-tools-version: 6.4
import PackageDescription

let package = Package(
    name: "HysteriaX",
    defaultLocalization: "en",
    platforms: [.macOS(.v27)],
    products: [.executable(name: "HysteriaX", targets: ["HysteriaX"])],
    targets: [
        .executableTarget(name: "HysteriaX", path: "Sources/HysteriaX", resources: [.process("Resources")])
    ]
)
