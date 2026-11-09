// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "XshApp",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(url: "https://github.com/migueldeicaza/SwiftTerm", exact: "1.99.2")
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
