// swift-tools-version:6.2

// SwiftTerm v1.99.2, vendored with one patch for xsh. See PATCHES.md.
// The upstream manifest also declares tests, tools, WASM targets and the
// Linux-only PNG dependency; xsh needs only the macOS library.

import PackageDescription

let package = Package(
    name: "SwiftTerm",
    platforms: [.macOS(.v11)],
    products: [
        .library(name: "SwiftTerm", targets: ["SwiftTerm"]),
    ],
    targets: [
        .target(
            name: "SwiftTerm",
            path: "Sources/SwiftTerm",
            exclude: [
                "Mac/README.md",
                "Apple/Metal/Shaders.metal",
            ],
            plugins: [
                .plugin(name: "SwiftTermBuildInfoPlugin")
            ]
        ),
        .executableTarget(
            name: "SwiftTermBuildInfoGenerator",
            path: "Sources/SwiftTermBuildInfoGenerator"
        ),
        .plugin(
            name: "SwiftTermBuildInfoPlugin",
            capability: .buildTool(),
            dependencies: ["SwiftTermBuildInfoGenerator"]
        ),
    ],
    swiftLanguageModes: [.v6]
)
