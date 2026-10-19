// SPDX-License-Identifier: Apache-2.0

import XCTest

@testable import AlbumenUpload

final class BackgroundUploadTests: XCTestCase {
    func testPutRequestKeepsThePasswordOutOfTheHeaders() throws {
        let secret = "receiver-password-not-in-the-request"
        let request = try BackgroundPut.request(
            endpoint: URL(string: "http://127.0.0.1:8080/library/")!,
            filename: "album/IMG_0001.JPG",
            mtimeSeconds: 1_700_000_000
        )
        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Albumen-Mtime"), "1700000000")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/octet-stream")
        XCTAssertEqual(request.url?.lastPathComponent, "IMG_0001.JPG")
        XCTAssertFalse(String(describing: request).contains(secret))
        XCTAssertThrowsError(try BackgroundPut.request(endpoint: URL(string: "http://127.0.0.1/")!, filename: "..", mtimeSeconds: 0))
    }

    func testBackgroundSessionAsksToBeRelaunched() throws {
        let configuration = BackgroundUpload.makeConfiguration()
        XCTAssertEqual(configuration.identifier, BackgroundUpload.sessionIdentifier)
        XCTAssertTrue(configuration.sessionSendsLaunchEvents)
        XCTAssertFalse(configuration.isDiscretionary)
    }

    func testMismatchNamesBothHashesAndAMatchCounts() {
        XCTAssertEqual(
            BackgroundOutcome.decide(
                localIdentifier: "asset",
                clientSha256: "aa",
                status: 201,
                serverSha256: "bb",
                transport: nil
            ),
            .failure("SHA-256 mismatch for asset. Sent aa. Receiver reported bb.")
        )
        XCTAssertEqual(
            BackgroundOutcome.decide(
                localIdentifier: "asset",
                clientSha256: "ab",
                status: 201,
                serverSha256: "AB",
                transport: nil
            ),
            .verified(sha256: "ab", alreadyStored: false)
        )
        XCTAssertEqual(
            BackgroundOutcome.decide(
                localIdentifier: "asset",
                clientSha256: "ab",
                status: 409,
                serverSha256: "ab",
                transport: nil
            ),
            .verified(sha256: "ab", alreadyStored: true)
        )
        XCTAssertEqual(
            BackgroundOutcome.decide(
                localIdentifier: "asset",
                clientSha256: "ab",
                status: 201,
                serverSha256: nil,
                transport: nil
            ),
            .failure("asset was not accepted. The receiver did not return X-Albumen-Sha256.")
        )
    }

    func testStatusAlwaysNamesTheFailureCount() {
        XCTAssertEqual(
            BackupStatusText.render(verifiedThisRun: 0, alreadyVerified: 0, failures: []),
            "Verified 0 this run. Already verified 0. Failures 0."
        )
        let text = BackupStatusText.render(
            verifiedThisRun: 1,
            alreadyVerified: 2,
            failures: ["asset still needs the full original (photo). A preview was not uploaded."]
        )
        XCTAssertTrue(text.contains("A preview was not uploaded."))
        XCTAssertTrue(text.hasSuffix("Verified 1 this run. Already verified 2. Failures 1."))
        let secret = "receiver-password-not-in-the-sentence"
        XCTAssertFalse(UploadFailure.missingPassword.surfaced(identifier: "Backup").contains(secret))
    }

    func testDerivativeStaysQueuedUntilThePersonAsksAgain() {
        var queue = UploadQueue()
        queue.noteDerivative(identifier: "id", message: "id still needs the full original. A preview was not uploaded.")
        XCTAssertEqual(queue.pending, ["id"])
        XCTAssertNotNil(queue.derivativeMessages["id"])
        queue.retryOnUserRequest()
        XCTAssertTrue(queue.derivativeMessages.isEmpty)
        XCTAssertEqual(queue.pending, ["id"])
    }

    func testReceiverFileHasNoPasswordField() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try ReceiverEndpointStore.save(ReceiverEndpoint(endpoint: "http://127.0.0.1:8080/", user: "camera"), directory: directory)
        let data = try Data(contentsOf: directory.appendingPathComponent(ReceiverEndpointStore.fileName))
        let text = String(decoding: data, as: UTF8.self)
        XCTAssertFalse(text.localizedCaseInsensitiveContains("password"))
        let loaded = try ReceiverEndpointStore.load(directory: directory)
        XCTAssertEqual(loaded.user, "camera")
        try? FileManager.default.removeItem(at: directory)
    }

    func testScratchRemovalStaysInsideTheScratchDirectory() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let file = root.appendingPathComponent("original.jpg")
        try Data("jpeg".utf8).write(to: file)
        try BackgroundScratch.remove(scratch: root, file: file)
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        let outside = FileManager.default.temporaryDirectory.appendingPathComponent("not-scratch-\(UUID().uuidString)")
        try Data("x".utf8).write(to: outside)
        XCTAssertThrowsError(try BackgroundScratch.remove(scratch: root, file: outside))
        XCTAssertTrue(FileManager.default.fileExists(atPath: outside.path))
        try? FileManager.default.removeItem(at: outside)
        try? FileManager.default.removeItem(at: root)
    }

    func testKeychainGroupIsRecordedWithoutThePassword() {
        let secret = "receiver-password-not-in-the-lookup"
        let query = ReceiverPassword.lookupQuery(accessGroup: KeychainAccess.groupSuffix)
        let rendered = query.values.map { String(describing: $0) }.joined(separator: " ")
        XCTAssertFalse(rendered.contains(secret))
        XCTAssertEqual(query[kSecAttrAccessGroup as String] as? String, KeychainAccess.groupSuffix)
    }
}
