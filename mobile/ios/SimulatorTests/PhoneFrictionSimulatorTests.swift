// SPDX-License-Identifier: Apache-2.0

import CryptoKit
import Darwin
import Foundation
import XCTest

import AlbumenUpload

/// Counts the shared step list from install through the pairing QR to a
/// SHA-256-verified copy. The list is the machine-readable source. More than
/// five steps fails. There is no account. The bytes are synthetic.
final class PhoneFrictionSimulatorTests: XCTestCase {
    private let user = "camera"

    func testPairingQrReachesAVerifiedCopyWithinFiveSteps() async throws {
        let steps = try loadSteps()
        XCTAssertLessThanOrEqual(steps.count, 5, "phone friction is \(steps.count) steps")
        XCTAssertFalse(steps.contains { $0.contains("account") || $0.contains("login") || $0.contains("signup") })
        let fingerprint = String(repeating: "ab", count: 32)
        let token = String(repeating: "cd", count: 32)
        let jpeg = SyntheticGpsJpeg.bytes()
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("albumen-friction-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let file = directory.appendingPathComponent("albumen-synthetic-pair.jpg")
        try jpeg.write(to: file)
        let server = try VerifiedReceiver(expectedBasic: basicToken(user: user, password: token))
        defer { server.stop() }
        let https = server.endpoint.replacingOccurrences(of: "http://", with: "https://")
        let uri = "albumen-pair://v1?endpoint=\(percentEncode(https))&fingerprint=\(fingerprint)&token=\(token)"
        var performed: [String] = []
        func step(_ id: String) {
            let expected = performed.count < steps.count ? steps[performed.count] : nil
            XCTAssertEqual(expected, id, "step \(performed.count + 1)")
            performed.append(id)
        }
        let started = Date()
        XCTAssertEqual(Bundle.main.bundleIdentifier, "photos.albumen.ios.simulator-host")
        step("install")
        step("open")
        step("grant-media-and-location")
        let code = try Pairing.parse(uri)
        XCTAssertNil(Pairing.pin(expected: code.fingerprint, presented: fingerprint))
        XCTAssertEqual(code.token, token)
        step("scan-pairing-qr")
        XCTAssertEqual(performed, steps)
        try ReceiverPassword.save(token)
        let status = await ForegroundUpload.uploadOriginal(
            file: file,
            filename: "albumen-synthetic-pair.jpg",
            localIdentifier: "albumen-synthetic-pair.jpg",
            mtimeSeconds: 1_700_000_000,
            endpoint: URL(string: server.endpoint)!,
            user: user,
            accessGroup: nil,
            directory: directory
        )
        let elapsed = Date().timeIntervalSince(started)
        XCTAssertLessThan(elapsed, 5 * 60, "first verified copy took \(elapsed)s. status: \(status)")
        XCTAssertEqual(server.body, jpeg)
        XCTAssertTrue(server.header.contains(basicToken(user: user, password: token)), server.header)
        XCTAssertTrue(status.contains("Verified 1 this run."), status)
        XCTAssertTrue(status.contains("Failures 0."), status)
        XCTAssertFalse(status.lowercased().contains("account"), status)
        let source = try Data(contentsOf: file)
        XCTAssertEqual(source, jpeg)
    }

    private func loadSteps() throws -> [String] {
        let url = try XCTUnwrap(
            Bundle(for: PhoneFrictionSimulatorTests.self).url(forResource: "phone-friction-steps", withExtension: "txt")
        )
        let text = try String(contentsOf: url, encoding: .utf8)
        return text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }.filter { line in
            !line.isEmpty && !line.hasPrefix("#")
        }
    }

    private func percentEncode(_ text: String) -> String {
        var out = ""
        for byte in text.utf8 {
            let value = Int(byte)
            let scalar = UnicodeScalar(value)!
            if CharacterSet.alphanumerics.contains(scalar) || "-._~".contains(scalar) {
                out.append(Character(scalar))
            } else {
                out.append(String(format: "%%%02X", value))
            }
        }
        return out
    }

    private func basicToken(user: String, password: String) -> String {
        Data("\(user):\(password)".utf8).base64EncodedString()
    }
}

/// Loopback that challenges for the pairing token, then answers 201 with the
/// SHA-256 of the bytes it received. The foreground upload omits Authorization
/// until this process sends a Basic challenge. A missing Content-Length on the
/// `Expect: 100-continue` flight still has a body after 100 Continue.
private final class VerifiedReceiver {
    let endpoint: String
    private let expectedBasic: String
    private let listenFD: Int32
    private let queue = DispatchQueue(label: "albumen-friction-receiver")
    private let lock = NSLock()
    private var storedHeader = ""
    private var storedBody = Data()

    var header: String {
        lock.lock()
        defer { lock.unlock() }
        return storedHeader
    }

    var body: Data {
        lock.lock()
        defer { lock.unlock() }
        return storedBody
    }

    init(expectedBasic: String) throws {
        self.expectedBasic = expectedBasic
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        if fd < 0 {
            throw POSIXError(.ECONNREFUSED)
        }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout.size(ofValue: yes)))
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = 0
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if bound != 0 || listen(fd, 4) != 0 {
            close(fd)
            throw POSIXError(.EADDRINUSE)
        }
        var got = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &got) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(fd, $0, &len)
            }
        }
        if named != 0 {
            close(fd)
            throw POSIXError(.EADDRINUSE)
        }
        listenFD = fd
        endpoint = "http://127.0.0.1:\(UInt16(bigEndian: got.sin_port))/"
        queue.async { [weak self] in
            self?.serve()
        }
    }

    func stop() {
        close(listenFD)
    }

    private func serve() {
        while true {
            let client = accept(listenFD, nil, nil)
            if client < 0 {
                break
            }
            respond(client)
            close(client)
        }
    }

    private func respond(_ client: Int32) {
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        let (header, leftover) = readHeaders(client)
        let text = String(data: header, encoding: .isoLatin1) ?? ""
        let declared = contentLength(text)
        let chunked = headerValue(text, name: "transfer-encoding")?.lowercased().contains("chunked") ?? false
        let expectsContinue = headerValue(text, name: "expect")?.lowercased().contains("100-continue") == true
            || text.lowercased().contains("100-continue")
        if expectsContinue && leftover.count < (declared ?? Int.max) {
            writeAll(client, Data("HTTP/1.1 100 Continue\r\n\r\n".utf8))
        }
        let body = readBody(client, declared: declared, chunked: chunked, expectsContinue: expectsContinue, leftover: leftover)
        guard text.contains(expectedBasic) else {
            writeAll(
                client,
                Data("HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"albumen\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".utf8)
            )
            return
        }
        let digest = SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined()
        lock.lock()
        storedHeader = text
        storedBody = body
        lock.unlock()
        writeAll(
            client,
            Data("HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\nX-Albumen-Sha256: \(digest)\r\n\r\n".utf8)
        )
    }

    private func readHeaders(_ client: Int32) -> (Data, Data) {
        var buf = Data()
        let marker = Data([13, 10, 13, 10])
        while buf.count < 64 * 1024 {
            let chunk = readSome(client, max: 4096)
            if chunk.isEmpty {
                break
            }
            buf.append(chunk)
            if let range = buf.range(of: marker) {
                let header = buf.subdata(in: buf.startIndex..<range.lowerBound)
                let leftover = buf.subdata(in: range.upperBound..<buf.endIndex)
                return (header, leftover)
            }
        }
        return (buf, Data())
    }

    private func readBody(
        _ client: Int32,
        declared: Int?,
        chunked: Bool,
        expectsContinue: Bool,
        leftover: Data
    ) -> Data {
        if chunked {
            return decodeChunks(readUntilPause(client, already: leftover))
        }
        if let declared, declared > 0 {
            return readExact(client, count: declared, already: leftover)
        }
        if declared == 0, !expectsContinue, leftover.isEmpty {
            return Data()
        }
        return readUntilPause(client, already: leftover)
    }

    private func readExact(_ client: Int32, count: Int, already: Data) -> Data {
        var body = Data(already.prefix(count))
        while body.count < count {
            let chunk = readSome(client, max: count - body.count)
            if chunk.isEmpty {
                break
            }
            body.append(chunk)
        }
        return body
    }

    private func readUntilPause(_ client: Int32, already: Data) -> Data {
        var pause = timeval(tv_sec: 0, tv_usec: 400_000)
        setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &pause, socklen_t(MemoryLayout<timeval>.size))
        var body = already
        while body.count < 8 * 1024 * 1024 {
            let chunk = readSome(client, max: 8192)
            if chunk.isEmpty {
                break
            }
            body.append(chunk)
            if chunk.count < 8192 {
                let extra = readSome(client, max: 8192)
                if extra.isEmpty {
                    break
                }
                body.append(extra)
            }
        }
        return body
    }

    private func readSome(_ client: Int32, max: Int) -> Data {
        var buf = [UInt8](repeating: 0, count: max)
        let n = buf.withUnsafeMutableBytes { raw -> Int in
            guard let base = raw.baseAddress else {
                return -1
            }
            return read(client, base, raw.count)
        }
        if n <= 0 {
            return Data()
        }
        return Data(buf.prefix(n))
    }

    private func writeAll(_ client: Int32, _ bytes: Data) {
        bytes.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else {
                return
            }
            var sent = 0
            while sent < raw.count {
                let n = write(client, base.advanced(by: sent), raw.count - sent)
                if n <= 0 {
                    break
                }
                sent += n
            }
        }
    }

    private func contentLength(_ header: String) -> Int? {
        guard let raw = headerValue(header, name: "content-length") else {
            return nil
        }
        return Int(raw)
    }

    private func headerValue(_ header: String, name: String) -> String? {
        let normalized = header.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        for line in normalized.split(separator: "\n", omittingEmptySubsequences: false) {
            let parts = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2 else {
                continue
            }
            let key = parts[0].trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if key == name {
                return parts[1].trimmingCharacters(in: .whitespacesAndNewlines)
            }
        }
        return nil
    }

    private func decodeChunks(_ raw: Data) -> Data {
        var out = Data()
        var index = raw.startIndex
        while index < raw.endIndex {
            guard let lineEnd = raw[index...].firstIndex(of: 10) else {
                break
            }
            let line = raw[index..<lineEnd].filter { $0 != 13 }
            let hex = String(bytes: line, encoding: .ascii)?
                .split(separator: ";")
                .first
                .map(String.init)?
                .trimmingCharacters(in: .whitespaces) ?? ""
            guard let size = Int(hex, radix: 16) else {
                break
            }
            var next = raw.index(after: lineEnd)
            if size == 0 {
                break
            }
            guard let end = raw.index(next, offsetBy: size, limitedBy: raw.endIndex) else {
                out.append(raw[next..<raw.endIndex])
                break
            }
            out.append(raw[next..<end])
            next = end
            if next < raw.endIndex, raw[next] == 13 {
                next = raw.index(after: next)
            }
            if next < raw.endIndex, raw[next] == 10 {
                next = raw.index(after: next)
            }
            index = next
        }
        return out
    }
}
