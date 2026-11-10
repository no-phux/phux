// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "RuneTailnet",
    platforms: [.iOS("26.0"), .macOS("26.0")],
    products: [.library(name: "RuneTailnet", type: .static, targets: ["RuneTailnet"])],
    dependencies: [
        .package(url: "https://github.com/apple/swift-nio.git", exact: "2.104.0"),
        .package(url: "https://github.com/apple/swift-nio-ssh.git", exact: "0.15.0")
    ],
    targets: [
        .binaryTarget(
            name: "TailscaleKit",
            url: "https://github.com/no-phux/libtailscale/releases/download/"
                + "tailscalekit-3af8bca979a5/TailscaleKit-3af8bca979a5.xcframework.zip",
            checksum: "610fe434360393508d61138da3ba86ca01c4640dc6e95341c1ec1dcf4e07631d"
        ),
        // Scope strict warnings to our targets: SwiftBuild suppresses dependency
        // warnings, which conflicts with globally forwarded -warnings-as-errors.
        .target(name: "RuneTailnet", dependencies: [
            "TailscaleKit",
            .product(name: "NIOCore", package: "swift-nio"),
            .product(name: "NIOPosix", package: "swift-nio"),
            .product(name: "NIOSSH", package: "swift-nio-ssh")
        ], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])]),
        .testTarget(name: "RuneTailnetTests", dependencies: [
            "RuneTailnet",
            .product(name: "NIOCore", package: "swift-nio"),
            .product(name: "NIOPosix", package: "swift-nio"),
            .product(name: "NIOSSH", package: "swift-nio-ssh")
        ], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])])
    ]
)
