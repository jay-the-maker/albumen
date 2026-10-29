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
        XCTAssertEqual(
            UploadCheck.verdict(clientSha256: "aa", status: 403, serverSha256: nil),
            .rejected(status: 403)
        )
        XCTAssertThrowsError(try UploadCheck.requireVerified(.missingServerHash))
    }

    func testKeychainLookupOmitsThePassword() throws {
        let secret = "receiver-password-not-in-the-lookup"
        let query = ReceiverPassword.lookupQuery()
        let rendered = query.values.map { String(describing: $0) }.joined(separator: " ")
        XCTAssertFalse(rendered.contains(secret))
        XCTAssertEqual(query[kSecClass as String] as? String, kSecClassGenericPassword as String)
        try ReceiverPassword.save(secret)
        XCTAssertEqual(try ReceiverPassword.read(), secret)
        let loaded = try WebDavUploader.fromKeychain(
            endpoint: URL(string: "https://albumen.example")!,
            user: "camera"
        )
        XCTAssertEqual(loaded.password, secret)
        XCTAssertEqual(loaded.user, "camera")
    }

    func testGpsStrippedOriginalIsRefusedLikeADerivative() {
        XCTAssertEqual(
            LocationCheck.refusal(assetHasLocation: true, originalHasGPS: false),
            .derivativeOnly(LocationCheck.strippedDetail)
        )
        XCTAssertNil(LocationCheck.refusal(assetHasLocation: true, originalHasGPS: true))
        XCTAssertNil(LocationCheck.refusal(assetHasLocation: false, originalHasGPS: false))
        let message = UploadFailure.derivativeOnly(LocationCheck.strippedDetail).surfaced(identifier: "IMG_0001")
        XCTAssertTrue(message.contains("still needs the full original"), message)
    }

    func testRemoteAssetDoesNotFallBackToADerivative() {
        XCTAssertEqual(
            OriginalPolicy.choose(role: .still, present: [.photo], isRemote: true),
            .derivativeOnly(present: [.photo])
        )
        XCTAssertEqual(
            OriginalPolicy.choose(role: .video, present: [.video], isRemote: true),
            .derivativeOnly(present: [.video])
        )
        XCTAssertEqual(
            OriginalPolicy.choose(role: .pairedVideo, present: [.pairedVideo], isRemote: true),
            .derivativeOnly(present: [.pairedVideo])
        )
    }

    func testFullSizeOriginalWinsAndALocalPhotoCanStandAlone() {
        XCTAssertEqual(
            OriginalPolicy.choose(
                role: .still,
                present: [.photo, .fullSizePhoto],
                isRemote: true
            ),
            .use(.fullSizePhoto)
        )
        XCTAssertEqual(
            OriginalPolicy.choose(role: .still, present: [.adjustmentBasePhoto], isRemote: true),
            .use(.adjustmentBasePhoto)
        )
        XCTAssertEqual(
            OriginalPolicy.choose(role: .still, present: [.photo], isRemote: false),
            .use(.photo)
        )
        XCTAssertEqual(
            OriginalPolicy.choose(role: .video, present: [.fullSizeVideo, .video], isRemote: true),
            .use(.fullSizeVideo)
        )
        XCTAssertEqual(
            OriginalPolicy.choose(role: .pairedVideo, present: [.fullSizePairedVideo], isRemote: true),
            .use(.fullSizePairedVideo)
        )
    }

    func testSourceMtimeHeaderIsUnixSeconds() {
        let date = Date(timeIntervalSince1970: 1_000_000_000)
        XCTAssertEqual(SourceMtime.headerValue(modificationDate: date), "1000000000")
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
