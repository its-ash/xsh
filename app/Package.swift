// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "XshApp",
    platforms: [.macOS(.v14)],
    dependencies: [
        // SwiftTerm v1.99.2 with a fix for typing lag in full screen; see
        // Vendor/SwiftTerm/PATCHES.md before you upgrade it.
        .package(path: "Vendor/SwiftTerm")
    ],
    targets: [
        .executableTarget(
            name: "XshApp",
            dependencies: [.product(name: "SwiftTerm", package: "SwiftTerm")],
            path: "Sources/XshApp",
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [.linkedFramework("Carbon")]
        )
    ]
)
