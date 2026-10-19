// SPDX-License-Identifier: Apache-2.0

import XCTest

final class SourcePolicyTests: XCTestCase {
    func testSourcesDoNotDeletePhotosOrRenderAPreview() throws {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let directories = ["Sources", "App", "BackgroundUpload", "Shared"].map { root.appendingPathComponent($0) }
        let forbidden = [
            "deleteAssets(",
            "PHAssetChangeRequest(",
            "performChanges(",
            "requestImage(",
            "UIImage(",
            "jpegData(",
        ]
        let removeItemAllowed: Set<String> = [
            "CameraRollBackup.swift",
            "BackgroundUpload.swift",
        ]
        var sawSwift = false
        for directory in directories {
            guard let enumerator = FileManager.default.enumerator(at: directory, includingPropertiesForKeys: nil) else {
                XCTFail("missing \(directory.path)")
                continue
            }
            for case let url as URL in enumerator {
                guard url.pathExtension == "swift" else {
                    continue
                }
                sawSwift = true
                let text = try String(contentsOf: url, encoding: .utf8)
                for token in forbidden where text.contains(token) {
                    XCTFail("\(url.lastPathComponent) contains \(token)")
                }
                if text.contains("removeItem"), !removeItemAllowed.contains(url.lastPathComponent) {
                    XCTFail("\(url.lastPathComponent) deletes a file")
                }
            }
        }
        XCTAssertTrue(sawSwift)
    }

    func testPlistsDoNotCarryTheReceiverPassword() throws {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let directories = ["App", "BackgroundUpload"].map { root.appendingPathComponent($0) }
        var sawPlist = false
        for directory in directories {
            let contents = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            for url in contents where url.pathExtension == "plist" || url.pathExtension == "entitlements" {
                sawPlist = true
                let text = try String(contentsOf: url, encoding: .utf8)
                XCTAssertFalse(text.localizedCaseInsensitiveContains("password"), url.lastPathComponent)
            }
        }
        XCTAssertTrue(sawPlist)
    }
}
