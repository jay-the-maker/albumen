// SPDX-License-Identifier: Apache-2.0

import CryptoKit
import Darwin
import Foundation
import UserNotifications
import XCTest

import AlbumenUpload

/// Seeded soak of synthetic items on the iOS Simulator. The Photos library is
/// not opened. Bytes live in the test directory. The receiver stores only the
/// uploaded filename. A `members/` path is refused.
///
/// Power loss closes the socket and ends the pass. The next pass continues
/// with the items that are still unverified. That is the stand-in for a killed
/// upload process. Rebooting the simulator on every fault would not finish the
/// shard.
///
/// The pull-request simulator job does not arm this test. The vector check
/// still runs.
final class SimulatorScaleSoakTests: XCTestCase {
    func testScheduleMatchesTheSharedVectors() {
        let expected = [
            "network-drop",
            "power-loss",
            "receiver-down",
            "sha-mismatch",
            "network-drop",
            "healthy",
            "receiver-down",
            "healthy",
        ]
        let got = (0..<expected.count).map { ScaleSoakSchedule.fault(seed: 1, localIndex: $0, globalIndex: $0) }
        XCTAssertEqual(got, expected)
        XCTAssertEqual(ScaleSoakSchedule.fault(seed: 1, localIndex: 5, globalIndex: 1_005), "network-drop")
    }

    func testSeededFaultsLeaveNoSilentFailure() async throws {
        guard let plan = SoakPlan.load() else {
            return
        }
        let items = plan.items
        let shard = plan.shard
        let shards = plan.shards
        let seed = plan.seed
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let sources = root.appendingPathComponent("sources", isDirectory: true)
        try FileManager.default.createDirectory(at: sources, withIntermediateDirectories: true)
        var originals: [Int: Data] = [:]
        for local in 0..<items {
            let global = shard * items + local
            var bytes = Data(count: 128)
            for index in 0..<bytes.count {
                bytes[index] = UInt8((global + index) & 0xff)
            }
            originals[local] = bytes
            try bytes.write(to: sources.appendingPathComponent(filename(shard: shard, local: local)))
        }
        let server = try ScaleSoakSocket(library: root.appendingPathComponent("library", isDirectory: true))
        defer { server.stop() }
        let downPort = refusedPort()
        try ReceiverPassword.save("albumen-ci-not-a-secret")
        let started = Date()
        var verified = Set<Int>()
        var faulted = Set<Int>()
        var silent = 0
        var faultPasses = 0
        var timeTo = -1
        var timeline = "elapsed_ms,verified,items,silent_failures\n"
        let threshold = (items * 999 + 999) / 1000
        func note() {
            let elapsed = Int(Date().timeIntervalSince(started) * 1000)
            timeline += "\(elapsed),\(verified.count),\(items),\(silent)\n"
            if timeTo < 0, verified.count >= threshold {
                timeTo = elapsed
            }
        }
        while faulted.count < items {
            faultPasses += 1
            let passDir = root.appendingPathComponent("pass-\(faultPasses)", isDirectory: true)
            try FileManager.default.createDirectory(at: passDir, withIntermediateDirectories: true)
            var attempted: [Int] = []
            var stop = false
            for local in 0..<items {
                if stop || faulted.contains(local) || verified.contains(local) {
                    continue
                }
                let global = shard * items + local
                let kind = ScaleSoakSchedule.fault(seed: seed, localIndex: local, globalIndex: global)
                faulted.insert(local)
                attempted.append(local)
                let ok = try await upload(
                    local: local,
                    shard: shard,
                    kind: kind,
                    server: server,
                    downPort: downPort,
                    sources: sources,
                    directory: passDir
                )
                if ok {
                    verified.insert(local)
                    note()
                } else {
                    let status = BackupReportStore.readStatus(directory: passDir)
                    let name = filename(shard: shard, local: local)
                    if !status.contains(name) {
                        silent += 1
                    }
                    XCTAssertFalse(status.isEmpty)
                }
                if kind == "power-loss" {
                    stop = true
                }
            }
            XCTAssertFalse(attempted.isEmpty, "fault pass \(faultPasses) made no progress")
            let status = BackupReportStore.readStatus(directory: passDir)
            if status.contains("Failures 0.") == false {
                let notice = await postNotice(status)
                XCTAssertFalse(notice.isEmpty, "notification was blank after fault pass \(faultPasses)")
                for local in attempted where !verified.contains(local) {
                    let name = filename(shard: shard, local: local)
                    XCTAssertTrue(
                        status.contains(name) || notice.contains(name),
                        "\(name) was unverified and absent from the status file and the notification"
                    )
                }
            }
            note()
        }
        let recoveryDir = root.appendingPathComponent("recovery", isDirectory: true)
        try FileManager.default.createDirectory(at: recoveryDir, withIntermediateDirectories: true)
        for local in 0..<items where !verified.contains(local) {
            let ok = try await upload(
                local: local,
                shard: shard,
                kind: "healthy",
                server: server,
                downPort: downPort,
                sources: sources,
                directory: recoveryDir
            )
            if ok {
                verified.insert(local)
                note()
            } else {
                let name = filename(shard: shard, local: local)
                let status = BackupReportStore.readStatus(directory: recoveryDir)
                if !status.contains(name) {
                    silent += 1
                }
            }
        }
        note()
        let elapsed = Int(Date().timeIntervalSince(started) * 1000)
        let line = resultLine(
            platform: "ios",
            shard: shard,
            shards: shards,
            seed: seed,
            verified: verified.count,
            items: items,
            elapsed: elapsed,
            timeTo: timeTo,
            silent: silent,
            faultPasses: faultPasses
        )
        var faults = "seed=\(seed)\nshard=\(shard)\nshards=\(shards)\nitems=\(items)\n"
        for local in 0..<items {
            let global = shard * items + local
            let name = filename(shard: shard, local: local)
            let kind = ScaleSoakSchedule.fault(seed: seed, localIndex: local, globalIndex: global)
            faults += "\(local) \(global) \(name) \(kind)\n"
        }
        try writeOutputs(line: line, faults: faults, timeline: timeline)
        print(line)
        for local in 0..<items {
            let name = filename(shard: shard, local: local)
            let source = try Data(contentsOf: sources.appendingPathComponent(name))
            XCTAssertEqual(source, originals[local])
            let stored = try Data(contentsOf: server.library.appendingPathComponent(name))
            XCTAssertEqual(stored, originals[local])
        }
        for path in server.paths {
            XCTAssertFalse(path.contains("..") || path.contains("members/"), path)
        }
        XCTAssertEqual(silent, 0)
        XCTAssertEqual(verified.count, items)
        XCTAssertGreaterThanOrEqual(timeTo, 0)
        XCTAssertTrue(line.contains("silent_failures=0"))
    }

    private func upload(
        local: Int,
        shard: Int,
        kind: String,
        server: ScaleSoakSocket,
        downPort: UInt16,
        sources: URL,
        directory: URL
    ) async throws -> Bool {
        let name = filename(shard: shard, local: local)
        let file = sources.appendingPathComponent(name)
        let before = try BackupReportStore.load(directory: directory)
        let endpoint: URL
        if kind == "receiver-down" {
            endpoint = URL(string: "http://127.0.0.1:\(downPort)/")!
            server.setFault("healthy")
        } else {
            endpoint = URL(string: server.endpoint)!
            server.setFault(kind)
            server.expect(name)
        }
        let status = await ForegroundUpload.uploadOriginal(
            file: file,
            filename: name,
            localIdentifier: name,
            mtimeSeconds: 1_700_000_000 + shard * 1_000_000 + local,
            endpoint: endpoint,
            user: "camera",
            accessGroup: nil,
            directory: directory
        )
        let after = try BackupReportStore.load(directory: directory)
        let ok = after.verifiedThisRun > before.verifiedThisRun && after.failures.count == before.failures.count
        if !ok {
            XCTAssertTrue(status.contains(name), status)
        }
        return ok
    }

    private func filename(shard: Int, local: Int) -> String {
        String(format: "scale-s%02d-i%06d.bin", shard, local)
    }

    private func resultLine(
        platform: String,
        shard: Int,
        shards: Int,
        seed: UInt64,
        verified: Int,
        items: Int,
        elapsed: Int,
        timeTo: Int,
        silent: Int,
        faultPasses: Int
    ) -> String {
        let scaled = verified * 1000 / items
        return "backup-scale-soak platform=\(platform) shard=\(shard) shards=\(shards) seed=\(seed) " +
            "items=\(items) verified=\(verified) verified_percent=\(scaled / 10).\(scaled % 10) " +
            "elapsed_ms=\(elapsed) time_to_99_9_ms=\(timeTo) silent_failures=\(silent) fault_passes=\(faultPasses)"
    }

    private func writeOutputs(line: String, faults: String, timeline: String) throws {
        let dirs = [
            URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("backup-scale-soak", isDirectory: true),
            URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Documents/backup-scale-soak", isDirectory: true),
        ]
        for dir in dirs {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try Data(line.appending("\n").utf8).write(to: dir.appendingPathComponent("result.txt"))
            try Data(faults.utf8).write(to: dir.appendingPathComponent("faults.log"))
            try Data(timeline.utf8).write(to: dir.appendingPathComponent("timeline.csv"))
        }
    }

    private func postNotice(_ message: String) async -> String {
        let catcher = ScaleNoticeCatcher()
        let center = UNUserNotificationCenter.current()
        await MainActor.run { center.delegate = catcher }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            BackupNotice.post(message) { _ in continuation.resume() }
        }
        let deadline = Date().addingTimeInterval(3)
        var bodies = catcher.snapshot()
        while bodies.isEmpty, Date() < deadline {
            try? await Task.sleep(nanoseconds: 100_000_000)
            bodies = catcher.snapshot()
            if bodies.isEmpty {
                let delivered = await withCheckedContinuation { continuation in
                    center.getDeliveredNotifications { notes in
                        continuation.resume(returning: notes.map(\.request.content.body))
                    }
                }
                bodies.append(contentsOf: delivered)
            }
        }
        return bodies.joined(separator: "\n")
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

private struct SoakPlan {
    let items: Int
    let shard: Int
    let shards: Int
    let seed: UInt64

    static func load() -> SoakPlan? {
        let defaults = UserDefaults.standard
        if defaults.string(forKey: "albumenSoakArmed") == "yes",
           defaults.integer(forKey: "albumenSoakItems") > 0 {
            return SoakPlan(
                items: defaults.integer(forKey: "albumenSoakItems"),
                shard: defaults.integer(forKey: "albumenSoakShard"),
                shards: max(defaults.integer(forKey: "albumenSoakShards"), 1),
                seed: UInt64(defaults.string(forKey: "albumenSoakSeed") ?? "1") ?? 1
            )
        }
        let urls = [
            URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("albumen-soak-plan.txt"),
            URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("tmp/albumen-soak-plan.txt"),
            URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Caches/albumen-soak-plan.txt"),
        ]
        for url in urls {
            guard let text = try? String(contentsOf: url, encoding: .utf8), text.contains("armed=yes") else {
                continue
            }
            var values: [String: String] = [:]
            for line in text.split(separator: "\n") {
                let parts = line.split(separator: "=", maxSplits: 1).map(String.init)
                if parts.count == 2 {
                    values[parts[0]] = parts[1]
                }
            }
            guard let items = Int(values["items"] ?? ""), items > 0 else {
                continue
            }
            return SoakPlan(
                items: items,
                shard: Int(values["shard"] ?? "0") ?? 0,
                shards: max(Int(values["shards"] ?? "1") ?? 1, 1),
                seed: UInt64(values["seed"] ?? "1") ?? 1
            )
        }
        return nil
    }
}

private enum ScaleSoakSchedule {
    static let order = ["network-drop", "power-loss", "receiver-down", "sha-mismatch"]

    static func fault(seed: UInt64, localIndex: Int, globalIndex: Int) -> String {
        if localIndex < order.count {
            return order[localIndex]
        }
        let slot = Int((mix(seed: seed, globalIndex: globalIndex) >> 1) % 20)
        if slot < order.count {
            return order[slot]
        }
        return "healthy"
    }

    private static func mix(seed: UInt64, globalIndex: Int) -> UInt64 {
        let add: UInt64 = 0x9E3779B97F4A7C15
        var state = seed ^ (UInt64(globalIndex) &* add)
        state &+= add
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58476D1CE4E5B9
        z = (z ^ (z >> 27)) &* 0x94D049BB133111EB
        return z ^ (z >> 31)
    }
}

private final class ScaleNoticeCatcher: NSObject, UNUserNotificationCenterDelegate {
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

/// In-process receiver. Healthy and mismatch read the body. Drop and power
/// loss close after 64 bytes. `members/` is refused and not stored.
private final class ScaleSoakSocket {
    let endpoint: String
    let library: URL
    private let listenFD: Int32
    private let queue = DispatchQueue(label: "albumen-scale-soak")
    private let lock = NSLock()
    private var fault = "healthy"
    private var expected = "/"
    private var seen: [String] = []

    var paths: [String] {
        lock.lock()
        defer { lock.unlock() }
        return seen
    }

    func setFault(_ value: String) {
        lock.lock()
        fault = value
        lock.unlock()
    }

    func expect(_ name: String) {
        lock.lock()
        expected = "/" + name
        lock.unlock()
    }

    init(library: URL) throws {
        self.library = library
        try FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
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
        if bound != 0 || listen(fd, 8) != 0 {
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
        var timeout = timeval(tv_sec: 20, tv_usec: 0)
        setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        let (header, leftover) = readHeaders(client)
        let text = String(data: header, encoding: .isoLatin1) ?? ""
        let request = text.split(separator: "\r\n").first.map(String.init) ?? ""
        let path = request.split(separator: " ").dropFirst().first.map(String.init) ?? ""
        lock.lock()
        seen.append(path)
        let kind = fault
        let wanted = expected
        lock.unlock()
        let declared = contentLength(text)
        if text.lowercased().contains("100-continue"), leftover.count < (declared ?? Int.max) {
            writeAll(client, Data("HTTP/1.1 100 Continue\r\n\r\n".utf8))
        }
        if path.contains("..") || path.contains("members/") || path != wanted {
            _ = readExact(client, count: declared ?? leftover.count, already: leftover)
            writeAll(client, Data("HTTP/1.1 403 refusing a write outside this item\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".utf8))
            return
        }
        if kind == "network-drop" || kind == "power-loss" {
            _ = readLimit(client, already: leftover, limit: 64, declared: declared)
            return
        }
        let body: Data
        if let declared, declared > 0 {
            body = readExact(client, count: declared, already: leftover)
        } else {
            body = readUntilPause(client, already: leftover)
        }
        let digest: String
        if kind == "sha-mismatch" {
            digest = String(repeating: "0", count: 64)
        } else {
            digest = SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined()
        }
        let name = String(path.dropFirst())
        try? body.write(to: library.appendingPathComponent(name))
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
                return (
                    buf.subdata(in: buf.startIndex..<range.lowerBound),
                    buf.subdata(in: range.upperBound..<buf.endIndex)
                )
            }
        }
        return (buf, Data())
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

    /// URLSession may omit Content-Length until `100 Continue`. Do not hash that empty read.
    private func readUntilPause(_ client: Int32, already: Data) -> Data {
        var pause = timeval(tv_sec: 0, tv_usec: 400_000)
        setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &pause, socklen_t(MemoryLayout<timeval>.size))
        var body = already
        while body.count < 1024 * 1024 {
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

    private func readLimit(_ client: Int32, already: Data, limit: Int, declared: Int?) -> Data {
        let cap = min(limit, declared ?? limit)
        var body = Data(already.prefix(cap))
        while body.count < cap {
            let chunk = readSome(client, max: cap - body.count)
            if chunk.isEmpty {
                break
            }
            body.append(chunk)
        }
        return body
    }

    private func readSome(_ client: Int32, max: Int) -> Data {
        var buf = [UInt8](repeating: 0, count: max)
        let n = buf.withUnsafeMutableBytes { raw -> Int in
            guard let base = raw.baseAddress else { return -1 }
            return read(client, base, raw.count)
        }
        if n <= 0 {
            return Data()
        }
        return Data(buf.prefix(n))
    }

    private func writeAll(_ client: Int32, _ bytes: Data) {
        bytes.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
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
        let normalized = header.replacingOccurrences(of: "\r\n", with: "\n")
        for line in normalized.split(separator: "\n") {
            let parts = line.split(separator: ":", maxSplits: 1)
            guard parts.count == 2 else { continue }
            let key = parts[0].trimmingCharacters(in: .whitespaces).lowercased()
            if key == "content-length" {
                return Int(parts[1].trimmingCharacters(in: .whitespacesAndNewlines))
            }
        }
        return nil
    }
}
