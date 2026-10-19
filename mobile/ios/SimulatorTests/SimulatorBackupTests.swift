// SPDX-License-Identifier: Apache-2.0

import Darwin
import Foundation
import UserNotifications
import XCTest

import AlbumenUpload

/// Simulator coverage for setup. Background `URLSession` transfers do not
/// finish here, so the test drives `ForegroundUpload`, which uses the same
/// PUT, Keychain challenge, and SHA-256 verdict. The clock starts when the
/// endpoint and password are saved. A verified copy has to be on loopback
/// `albumen-receive` within five minutes, with the same Exif APP1 bytes.
/// A closed port and a wrong `X-Albumen-Sha256` have to be on the status
/// store the screen reads and in a local notification.
final class SimulatorBackupTests: XCTestCase {
    private let user = "camera"
    private let password = "albumen-ci-not-a-secret"
    private let receiver = URL(string: "http://127.0.0.1:18080/")!

    func testFirstVerifiedCopyLandsWithinFiveMinutesOfSetup() async throws {
        let directory = try makeDirectory()
        let name = "albumen-synthetic-first.jpg"
        let jpeg = SyntheticGpsJpeg.bytes()
        let sourceExif = SyntheticGpsJpeg.exifApp1(jpeg)
        let file = try write(jpeg, name: name, directory: directory)
        let started = Date()
        try ReceiverEndpointStore.save(
            ReceiverEndpoint(endpoint: receiver.absoluteString, user: user),
            directory: directory
        )
        try ReceiverPassword.save(password)
        let status = await ForegroundUpload.uploadOriginal(
            file: file,
            filename: name,
            localIdentifier: name,
            mtimeSeconds: 1_700_000_000,
            endpoint: receiver,
            user: user,
            accessGroup: nil,
            directory: directory
        )
        let stored = try await fetchStored(name)
        let elapsed = Date().timeIntervalSince(started)
        XCTAssertLessThan(
            elapsed,
            5 * 60,
            "first verified copy took \(elapsed)s after setup; the target is 5 minutes. status: \(status)"
        )
        XCTAssertEqual(stored, jpeg, "the file albumen-receive stored differs from the source")
        XCTAssertEqual(
            SyntheticGpsJpeg.exifApp1(stored),
            sourceExif,
            "GPS EXIF of the received file differs from the source"
        )
        let screen = BackupReportStore.readStatus(directory: directory)
        XCTAssertEqual(screen, status)
        XCTAssertTrue(screen.contains("Verified 1 this run."))
        XCTAssertTrue(screen.contains("Failures 0."))
    }

    func testHashMismatchSurfacesOnTheStatusAndTheNotification() async throws {
        let directory = try makeDirectory()
        let name = "albumen-synthetic-mismatch.jpg"
        let jpeg = SyntheticGpsJpeg.bytes()
        let file = try write(jpeg, name: name, directory: directory)
        let serverHash = String(repeating: "0", count: 64)
        let client = try Sha256File.hex(file)
        XCTAssertNotEqual(client, serverHash)
        let mismatch = try MismatchReceiver(sha256: serverHash)
        defer { mismatch.stop() }
        try ReceiverEndpointStore.save(
            ReceiverEndpoint(endpoint: mismatch.endpoint, user: user),
            directory: directory
        )
        try ReceiverPassword.save(password)
        let status = await ForegroundUpload.uploadOriginal(
            file: file,
            filename: name,
            localIdentifier: name,
            mtimeSeconds: 1_700_000_000,
            endpoint: URL(string: mismatch.endpoint)!,
            user: user,
            accessGroup: nil,
            directory: directory
        )
        let marker = "SHA-256 mismatch for \(name). Sent \(client). Receiver reported \(serverHash)."
        try await assertSurfaced(directory: directory, status: status, marker: marker)
    }

    func testReceiverDownSurfacesOnTheStatusAndTheNotification() async throws {
        let directory = try makeDirectory()
        let name = "albumen-synthetic-down.jpg"
        let jpeg = SyntheticGpsJpeg.bytes()
        let file = try write(jpeg, name: name, directory: directory)
        let endpoint = URL(string: "http://127.0.0.1:\(refusedPort())/")!
        try ReceiverEndpointStore.save(
            ReceiverEndpoint(endpoint: endpoint.absoluteString, user: user),
            directory: directory
        )
        try ReceiverPassword.save(password)
        let status = await ForegroundUpload.uploadOriginal(
            file: file,
            filename: name,
            localIdentifier: name,
            mtimeSeconds: 1_700_000_000,
            endpoint: endpoint,
            user: user,
            accessGroup: nil,
            directory: directory
        )
        let marker = "\(name) did not finish:"
        try await assertSurfaced(directory: directory, status: status, marker: marker)
    }

    private func assertSurfaced(directory: URL, status: String, marker: String) async throws {
        let screen = BackupReportStore.readStatus(directory: directory)
        XCTAssertEqual(screen, status)
        XCTAssertFalse(screen.isEmpty, "status screen was blank")
        let line = try XCTUnwrap(screen.split(separator: "\n").first { $0.contains(marker) })
        XCTAssertGreaterThan(failureCount(screen), 0, "status screen: \(screen)")
        XCTAssertFalse(screen.contains("Failures 0."))
        guard !screen.hasSuffix("Failures 0.") else {
            return
        }
        let notification = await postNotice(screen)
        XCTAssertFalse(notification.isEmpty, "notification was blank")
        XCTAssertTrue(
            notification.contains(line),
            "notification missing the status line [\(line)]. notification: \(notification)"
        )
        XCTAssertGreaterThan(failureCount(notification), 0, "notification: \(notification)")
        XCTAssertFalse(notification.contains("Failures 0."))
    }

    private func failureCount(_ text: String) -> Int {
        guard let match = text.range(of: #"Failures (\d+)\."#, options: .regularExpression) else {
            return -1
        }
        let slice = text[match]
        let digits = slice.filter(\.isNumber)
        return Int(digits) ?? -1
    }

    private func postNotice(_ message: String) async -> String {
        let catcher = NoticeCatcher()
        let center = UNUserNotificationCenter.current()
        let settings = await center.notificationSettings()
        let allowed: Set<UNAuthorizationStatus> = [.authorized, .provisional, .ephemeral]
        XCTAssertTrue(
            allowed.contains(settings.authorizationStatus),
            "notifications are \(settings.authorizationStatus.rawValue) for \(Bundle.main.bundleIdentifier ?? "unknown")"
        )
        await MainActor.run {
            center.delegate = catcher
        }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            BackupNotice.post(message) { _ in
                continuation.resume()
            }
        }
        let deadline = Date().addingTimeInterval(5)
        var bodies = catcher.snapshot()
        while bodies.isEmpty, Date() < deadline {
            try? await Task.sleep(nanoseconds: 200_000_000)
            bodies = catcher.snapshot()
            if bodies.isEmpty {
                let delivered = await withCheckedContinuation { continuation in
                    center.getDeliveredNotifications { notes in
                        continuation.resume(returning: notes.map(\.request.content.body))
                    }
                }
                bodies.append(contentsOf: delivered)
            }
            if bodies.isEmpty {
                let pending = await withCheckedContinuation { continuation in
                    center.getPendingNotificationRequests { requests in
                        continuation.resume(returning: requests.map(\.content.body))
                    }
                }
                bodies.append(contentsOf: pending)
            }
        }
        return bodies.joined(separator: "\n")
    }

    private func fetchStored(_ name: String) async throws -> Data {
        var request = URLRequest(url: receiver.appendingPathComponent(name))
        let token = Data("\(user):\(password)".utf8).base64EncodedString()
        request.setValue("Basic \(token)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await URLSession.shared.data(for: request)
        let http = try XCTUnwrap(response as? HTTPURLResponse)
        XCTAssertEqual(http.statusCode, 200, "GET \(name) status \(http.statusCode)")
        return data
    }

    private func makeDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func write(_ bytes: Data, name: String, directory: URL) throws -> URL {
        let url = directory.appendingPathComponent(name)
        try bytes.write(to: url)
        return url
    }

    private func refusedPort() -> UInt16 {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        precondition(fd >= 0)
        defer { close(fd) }
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
        precondition(bound == 0)
        var got = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &got) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(fd, $0, &len)
            }
        }
        precondition(named == 0)
        return UInt16(bigEndian: got.sin_port)
    }
}

private final class NoticeCatcher: NSObject, UNUserNotificationCenterDelegate {
    private let lock = NSLock()
    private var bodies: [String] = []

    func snapshot() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return bodies
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        lock.lock()
        bodies.append(notification.request.content.body)
        lock.unlock()
        completionHandler([])
    }
}

/// Speaks the receiver's 201 response with a digest that is not the file's.
private final class MismatchReceiver {
    let endpoint: String
    private let listenFD: Int32
    private let queue = DispatchQueue(label: "albumen-mismatch-receiver")
    private let sha256: String

    init(sha256: String) throws {
        self.sha256 = sha256
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
        var header = Data()
        var byte = [UInt8](repeating: 0, count: 1)
        while header.count < 64 * 1024 {
            let n = read(client, &byte, 1)
            if n <= 0 {
                break
            }
            header.append(byte[0])
            if header.suffix(4).elementsEqual([13, 10, 13, 10] as [UInt8]) {
                break
            }
        }
        let text = String(data: header, encoding: .isoLatin1) ?? ""
        let length = contentLength(text)
        if text.lowercased().contains("100-continue") {
            _ = "HTTP/1.1 100 Continue\r\n\r\n".withCString { reply in
                write(client, reply, strlen(reply))
            }
        }
        var body = Data()
        while body.count < length {
            var buf = [UInt8](repeating: 0, count: length - body.count)
            let n = read(client, &buf, buf.count)
            if n <= 0 {
                break
            }
            body.append(buf, count: n)
        }
        let response = "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\nX-Albumen-Sha256: \(sha256)\r\n\r\n"
        _ = response.withCString { reply in
            write(client, reply, strlen(reply))
        }
    }

    private func contentLength(_ header: String) -> Int {
        for line in header.split(separator: "\n") {
            let parts = line.split(separator: ":", maxSplits: 1)
            if parts.count == 2, parts[0].trimmingCharacters(in: .whitespaces).lowercased() == "content-length" {
                return Int(parts[1].trimmingCharacters(in: .whitespacesAndNewlines)) ?? 0
            }
        }
        return 0
    }
}
