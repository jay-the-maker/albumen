// SPDX-License-Identifier: Apache-2.0

import Darwin
import Foundation

/// Background WebDAV upload. The PUT we build has no Authorization header.
/// The session delegate reads the receiver password from the Keychain when
/// the receiver challenges, and the upload counts only when `X-Albumen-Sha256`
/// matches the file that was sent.
public enum BackgroundUpload {
    public static let sessionIdentifier = "photos.albumen.backup.background-upload"
    public static let appGroup = "group.photos.albumen"
    public static let refreshTaskIdentifier = "photos.albumen.backup.refresh"

    public static func makeConfiguration() -> URLSessionConfiguration {
        let configuration = URLSessionConfiguration.background(withIdentifier: sessionIdentifier)
        configuration.sessionSendsLaunchEvents = true
        configuration.isDiscretionary = false
        configuration.allowsCellularAccess = true
        return configuration
    }
}

public enum KeychainAccess {
    /// Shared group suffix. The entitlements prefix it with `$(AppIdentifierPrefix)`.
    public static let groupSuffix = "photos.albumen.shared"
    public static let infoKey = "AlbumenKeychainAccessGroup"

    /// The expanded access group, when the build substituted the team prefix.
    /// An unexpanded `$(...)` value means this process has no shared group yet.
    public static func group(bundle: Bundle = .main) -> String? {
        guard let raw = bundle.object(forInfoDictionaryKey: infoKey) as? String else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed.contains("$") {
            return nil
        }
        return trimmed
    }
}

public enum BackupContainer {
    public static func directory(fileManager: FileManager = .default) -> URL {
        if let group = fileManager.containerURL(forSecurityApplicationGroupIdentifier: BackgroundUpload.appGroup) {
            return group
        }
        let base = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? fileManager.temporaryDirectory
        let directory = base.appendingPathComponent("albumen-backup", isDirectory: true)
        try? fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }
}

/// The receiver address and user. The password is not a field here.
public struct ReceiverEndpoint: Codable, Equatable, Sendable {
    public var endpoint: String
    public var user: String

    public init(endpoint: String, user: String) {
        self.endpoint = endpoint
        self.user = user
    }
}

public enum ReceiverEndpointStore {
    public static let fileName = "albumen-receiver.json"

    public static func load(directory: URL) throws -> ReceiverEndpoint {
        let url = directory.appendingPathComponent(fileName)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return ReceiverEndpoint(endpoint: "", user: "")
        }
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode(ReceiverEndpoint.self, from: data)
    }

    public static func save(_ endpoint: ReceiverEndpoint, directory: URL) throws {
        let data = try JSONEncoder().encode(endpoint)
        try AtomicFile.write(data, to: directory.appendingPathComponent(fileName))
    }
}

public struct UploadQueue: Codable, Equatable, Sendable {
    public var pending: [String]
    public var derivativeMessages: [String: String]
    public var lastCreationTimestamp: Int?

    public init(
        pending: [String] = [],
        derivativeMessages: [String: String] = [:],
        lastCreationTimestamp: Int? = nil
    ) {
        self.pending = pending
        self.derivativeMessages = derivativeMessages
        self.lastCreationTimestamp = lastCreationTimestamp
    }

    public mutating func enqueue(_ identifier: String) {
        if !pending.contains(identifier) {
            pending.append(identifier)
        }
    }

    public mutating func noteDerivative(identifier: String, message: String) {
        enqueue(identifier)
        derivativeMessages[identifier] = message
    }

    /// A person asked for another pass, so derivative rows are tried again.
    public mutating func retryOnUserRequest() {
        derivativeMessages.removeAll()
    }
}

public enum UploadQueueStore {
    public static let fileName = "albumen-upload-queue.json"
    private static let lock = NSLock()

    public static func load(directory: URL) throws -> UploadQueue {
        lock.lock()
        defer { lock.unlock() }
        return try loadUnlocked(directory: directory)
    }

    public static func save(_ queue: UploadQueue, directory: URL) throws {
        lock.lock()
        defer { lock.unlock() }
        try saveUnlocked(queue, directory: directory)
    }

    public static func update(directory: URL, _ body: (inout UploadQueue) -> Void) throws {
        lock.lock()
        defer { lock.unlock() }
        var queue = try loadUnlocked(directory: directory)
        body(&queue)
        try saveUnlocked(queue, directory: directory)
    }

    private static func loadUnlocked(directory: URL) throws -> UploadQueue {
        let url = directory.appendingPathComponent(fileName)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return UploadQueue()
        }
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode(UploadQueue.self, from: data)
    }

    private static func saveUnlocked(_ queue: UploadQueue, directory: URL) throws {
        let data = try JSONEncoder().encode(queue)
        try AtomicFile.write(data, to: directory.appendingPathComponent(fileName))
    }
}

public struct PendingUpload: Codable, Equatable, Sendable {
    public let id: String
    public let localIdentifier: String
    public let filename: String
    public let clientSha256: String
    public let scratchPath: String

    public init(
        id: String,
        localIdentifier: String,
        filename: String,
        clientSha256: String,
        scratchPath: String
    ) {
        self.id = id
        self.localIdentifier = localIdentifier
        self.filename = filename
        self.clientSha256 = clientSha256
        self.scratchPath = scratchPath
    }
}

public enum PendingUploadStore {
    public static let fileName = "albumen-pending-uploads.json"
    private static let lock = NSLock()

    public static func load(directory: URL) throws -> [PendingUpload] {
        lock.lock()
        defer { lock.unlock() }
        return try loadUnlocked(directory: directory)
    }

    public static func update<T>(directory: URL, _ body: (inout [PendingUpload]) throws -> T) throws -> T {
        lock.lock()
        defer { lock.unlock() }
        var pending = try loadUnlocked(directory: directory)
        let result = try body(&pending)
        try saveUnlocked(pending, directory: directory)
        return result
    }

    private static func loadUnlocked(directory: URL) throws -> [PendingUpload] {
        let url = directory.appendingPathComponent(fileName)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return []
        }
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode([PendingUpload].self, from: data)
    }

    private static func saveUnlocked(_ pending: [PendingUpload], directory: URL) throws {
        let data = try JSONEncoder().encode(pending)
        try AtomicFile.write(data, to: directory.appendingPathComponent(fileName))
    }
}

public struct BackupReport: Codable, Equatable, Sendable {
    public var verifiedThisRun: Int
    public var alreadyVerified: Int
    public var failures: [String]

    public init(verifiedThisRun: Int = 0, alreadyVerified: Int = 0, failures: [String] = []) {
        self.verifiedThisRun = verifiedThisRun
        self.alreadyVerified = alreadyVerified
        self.failures = failures
    }

    public var text: String {
        BackupStatusText.render(
            verifiedThisRun: verifiedThisRun,
            alreadyVerified: alreadyVerified,
            failures: failures
        )
    }
}

public enum BackupStatusText {
    public static func render(verifiedThisRun: Int, alreadyVerified: Int, failures: [String]) -> String {
        let cleaned = failures
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        let summary = "Verified \(verifiedThisRun) this run. Already verified \(alreadyVerified). Failures \(cleaned.count)."
        return (cleaned + [summary]).joined(separator: "\n")
    }
}

public enum BackupReportStore {
    public static let reportFileName = "albumen-backup-report.json"
    public static let statusFileName = "albumen-backup-status.txt"
    private static let lock = NSLock()

    public static func load(directory: URL) throws -> BackupReport {
        let url = directory.appendingPathComponent(reportFileName)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return BackupReport()
        }
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode(BackupReport.self, from: data)
    }

    public static func update(directory: URL, _ body: (inout BackupReport) -> Void) throws -> BackupReport {
        lock.lock()
        defer { lock.unlock() }
        var report = try load(directory: directory)
        body(&report)
        let data = try JSONEncoder().encode(report)
        try AtomicFile.write(data, to: directory.appendingPathComponent(reportFileName))
        try AtomicFile.write(Data(report.text.utf8), to: directory.appendingPathComponent(statusFileName))
        return report
    }

    public static func readStatus(directory: URL) -> String {
        let url = directory.appendingPathComponent(statusFileName)
        guard let text = try? String(contentsOf: url, encoding: .utf8), !text.isEmpty else {
            return BackupReport().text
        }
        return text
    }
}

public enum MemberNamespace {
    /// `members/<ed25519-pubkey>/<filename>` under the family library root.
    /// An endpoint that is not that root is refused before a request is built.
    public static func uploadURL(endpoint: URL, memberId: String, filename: String) throws -> URL {
        let path = endpoint.path
        if path != "/" && !path.isEmpty {
            throw MemberNamespaceError(
                "backup endpoint is not the family library root"
            )
        }
        let id = try canonicalMemberId(memberId)
        let name = try StoredName.require(filename)
        return endpoint
            .appendingPathComponent("members")
            .appendingPathComponent(id)
            .appendingPathComponent(name)
    }

    public static func canonicalMemberId(_ raw: String) throws -> String {
        let id = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let hex = CharacterSet(charactersIn: "0123456789abcdef")
        guard id.count == 64, id.unicodeScalars.allSatisfy({ hex.contains($0) }) else {
            throw MemberNamespaceError("member id is not a 32-byte Ed25519 public key")
        }
        return id
    }

    public static func refusal(filename: String, memberId: String, detail: String) -> String {
        "Refusing a write of \(filename) outside members/\(memberId)/: \(detail). The camera roll was not changed."
    }
}

public struct MemberNamespaceError: Error, Equatable, CustomStringConvertible {
    public let message: String

    public init(_ message: String) {
        self.message = message
    }

    public var description: String { message }
}

public enum BackgroundPut {
    /// PUT an original. The Authorization header is absent so the password is
    /// not stored in the background session's request file. A family backup
    /// puts the original under `members/<memberId>/`.
    public static func request(
        endpoint: URL,
        filename: String,
        mtimeSeconds: Int,
        memberId: String? = nil
    ) throws -> URLRequest {
        let url: URL
        if let memberId, !memberId.isEmpty {
            url = try MemberNamespace.uploadURL(endpoint: endpoint, memberId: memberId, filename: filename)
        } else {
            let name = try StoredName.require(filename)
            url = endpoint.appendingPathComponent(name)
        }
        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.setValue(String(mtimeSeconds), forHTTPHeaderField: "X-Albumen-Mtime")
        return request
    }
}

public enum BackgroundOutcome: Equatable, Sendable {
    case verified(sha256: String, alreadyStored: Bool)
    case failure(String)

    public static func decide(
        localIdentifier: String,
        clientSha256: String,
        status: Int?,
        serverSha256: String?,
        transport: String?
    ) -> BackgroundOutcome {
        if let transport, !transport.isEmpty {
            return .failure(UploadFailure.transport(transport).surfaced(identifier: localIdentifier))
        }
        guard let status else {
            return .failure(UploadFailure.transport("no HTTP status").surfaced(identifier: localIdentifier))
        }
        let verdict = UploadCheck.verdict(
            clientSha256: clientSha256,
            status: status,
            serverSha256: serverSha256
        )
        switch verdict {
        case .verified(let sha):
            return .verified(sha256: sha, alreadyStored: status == 409)
        case .conflict(let client, let server):
            return .failure(UploadFailure.conflict(client: client, server: server).surfaced(identifier: localIdentifier))
        case .missingServerHash:
            return .failure(UploadFailure.missingServerHash.surfaced(identifier: localIdentifier))
        case .rejected(let code):
            return .failure(UploadFailure.rejected(status: code).surfaced(identifier: localIdentifier))
        }
    }
}

public enum BackgroundScratch {
    /// Delete a scratch file inside `scratch`. A path outside that directory is refused.
    /// This does not delete a Photos asset.
    public static func remove(scratch: URL, file: URL) throws {
        let root = scratch.standardizedFileURL.resolvingSymlinksInPath()
        let target = file.standardizedFileURL.resolvingSymlinksInPath()
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        guard target.path.hasPrefix(prefix) else {
            throw UploadFailure.badFilename(file.lastPathComponent)
        }
        if FileManager.default.fileExists(atPath: target.path) {
            try FileManager.default.removeItem(at: target)
        }
    }
}

enum AtomicFile {
    static func write(_ data: Data, to url: URL) throws {
        let directory = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let temp = directory.appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString)")
        try data.write(to: temp)
        let handle = try FileHandle(forWritingTo: temp)
        try handle.synchronize()
        try handle.close()
        if FileManager.default.fileExists(atPath: url.path) {
            _ = try FileManager.default.replaceItemAt(url, withItemAt: temp)
        } else {
            try FileManager.default.moveItem(at: temp, to: url)
        }
        let fd = directory.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(path, O_RDONLY)
        }
        guard fd >= 0 else {
            throw UploadFailure.transport("the backup directory was not saved")
        }
        _ = fsync(fd)
        _ = Darwin.close(fd)
    }
}
