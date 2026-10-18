// swift-tools-version:5.9
// SPDX-License-Identifier: Apache-2.0

import PackageDescription

let package = Package(
    name: "AlbumenBackup",
    platforms: [
        .iOS(.v17),
        .macOS(.v14),
    ],
    products: [
        .library(name: "AlbumenBackup", targets: ["AlbumenBackup"]),
    ],
    targets: [
        .target(name: "AlbumenUpload"),
        .target(
            name: "AlbumenBackup",
            dependencies: ["AlbumenUpload"]
        ),
        .testTarget(
            name: "AlbumenUploadTests",
            dependencies: ["AlbumenUpload"]
        ),
    ]
)
