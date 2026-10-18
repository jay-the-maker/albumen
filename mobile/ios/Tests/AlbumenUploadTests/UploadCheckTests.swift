// SPDX-License-Identifier: Apache-2.0

import XCTest

@testable import AlbumenUpload

final class UploadCheckTests: XCTestCase {
    func testCreatedCollisionAndIdenticalRetry() {
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "Ab", status: 201, serverSha256: "ab"),
            .verified(sha256: "ab")
        )
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "ab", status: 409, serverSha256: "ab"),
            .verified(sha256: "ab")
        )
    }

    func testMismatchIsAConflictAndAMissingHeaderIsNotSuccess() throws {
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "aa", status: 201, serverSha256: "bb"),
            .conflict(client: "aa", server: "bb")
        )
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "aa", status: 409, serverSha256: "bb"),
            .conflict(client: "aa", server: "bb")
        )
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "aa", status: 201, serverSha256: nil),
            .missingServerHash
        )
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "aa", status: 500, serverSha256: "aa"),
            .rejected(status: 500)
        )
        XCTAssertThrowsError(try UploadCheck.requireVerified(.missingServerHash))
    }

    func testFileDigestMatchesTheKnownVector() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data("abc".utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        XCTAssertEqual(
            try Sha256File.hex(url),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
    }

    func testStoredNameKeepsTheCameraFilename() throws {
        XCTAssertEqual(try StoredName.require("/DCIM/IMG_0001.HEIC"), "IMG_0001.HEIC")
        XCTAssertThrowsError(try StoredName.require("../IMG_0001.HEIC"))
    }
}
