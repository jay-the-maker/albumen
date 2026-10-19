// SPDX-License-Identifier: Apache-2.0

import XCTest

@testable import AlbumenUpload

/// Same URI vectors as `receive/src/pairing.rs` and the Android pairing tests.
final class PairingTests: XCTestCase {
    func testSharedVectorsParse() throws {
        let withMember = try Pairing.parse(Pairing.exampleWithMember)
        XCTAssertEqual(withMember.endpoint, "https://127.0.0.1:8443/")
        XCTAssertEqual(withMember.token, String(repeating: "a", count: 64))
        XCTAssertEqual(withMember.member, String(repeating: "b", count: 64))
        XCTAssertEqual(withMember.fingerprint.count, 64)

        let plain = try Pairing.parse(Pairing.exampleWithoutMember)
        XCTAssertEqual(plain.endpoint, "https://photos.example:8443/")
        XCTAssertNil(plain.member)
    }

    func testFingerprintMismatchIsVisibleAndAMatchAllowsTheCopy() throws {
        let code = try Pairing.parse(Pairing.exampleWithMember)
        XCTAssertNil(Pairing.pin(expected: code.fingerprint, presented: code.fingerprint.uppercased()))
        let wrong = String(repeating: "f", count: 64)
        let sentence = try XCTUnwrap(Pairing.pin(expected: code.fingerprint, presented: wrong))
        XCTAssertTrue(sentence.contains("fingerprint"))
        XCTAssertTrue(sentence.contains("The copy did not start."))
        XCTAssertTrue(sentence.contains(code.fingerprint))
        XCTAssertTrue(sentence.contains(wrong))
        XCTAssertFalse(sentence.contains("camera-secret"))
    }

    func testAWrongVersionDoesNotEchoTheToken() {
        let token = String(repeating: "a", count: 64)
        XCTAssertThrowsError(
            try Pairing.parse(
                "albumen-pair://v2?endpoint=https%3A%2F%2F127.0.0.1%2F&fingerprint=ab&token=\(token)"
            )
        ) { error in
            let rejection = error as? Pairing.Rejection
            XCTAssertTrue(rejection?.message.contains("did not start") == true)
            XCTAssertFalse(rejection?.message.contains(token) == true)
        }
    }

    func testCertificateBytesPinToTheKnownDigest() {
        let digest = Pairing.fingerprint(encodedCertificate: Data("abc".utf8))
        XCTAssertEqual(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
    }
}
