// swift-tools-version: 6.2
// WingMix: SwiftUI front end for the wing-mixd daemon (M051). Build the .app with build.sh.
import PackageDescription

let package = Package(
    name: "WingMix",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "WingMix", targets: ["WingMix"]),
    ],
    targets: [
        // UI-free logic (control socket client, models, daemon process management): testable.
        .target(
            name: "WingMixCore",
            swiftSettings: [.enableUpcomingFeature("NonisolatedNonsendingByDefault")]
        ),
        .executableTarget(
            name: "WingMix",
            dependencies: ["WingMixCore"],
            swiftSettings: [
                .defaultIsolation(MainActor.self),
                .enableUpcomingFeature("NonisolatedNonsendingByDefault"),
            ]
        ),
        .testTarget(
            name: "WingMixCoreTests",
            dependencies: ["WingMixCore"],
            exclude: ["Fixtures"]
        ),
    ]
)
