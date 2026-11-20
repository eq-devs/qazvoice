// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "qazvoice",
    platforms: [
        .iOS("15.1")
    ],
    products: [
        .library(name: "qazvoice", targets: ["qazvoice"])
    ],
    dependencies: [
        .package(name: "FlutterFramework", path: "../FlutterFramework")
    ],
    targets: [
        // libqazvoice (Rust engine + ONNX Runtime), loaded by dart:ffi.
        .binaryTarget(name: "QazVoiceNative", path: "Frameworks/QazVoiceNative.xcframework"),
        .target(
            name: "qazvoice",
            dependencies: [
                .product(name: "FlutterFramework", package: "FlutterFramework"),
                "QazVoiceNative",
            ],
            resources: [
                .process("PrivacyInfo.xcprivacy"),
            ],
            linkerSettings: [
                .linkedFramework("AVFoundation"),
                .linkedFramework("CoreML"),
            ]
        )
    ]
)
