// SPDX-License-Identifier: Apache-2.0

import Foundation
import Photos

import AlbumenUpload

public struct LibraryAsset: Equatable, Sendable {
    public let localIdentifier: String
    public let creationTimestamp: Int

    public init(localIdentifier: String, creationTimestamp: Int) {
        self.localIdentifier = localIdentifier
        self.creationTimestamp = creationTimestamp
    }
}

public struct ExportedOriginal: Equatable, Sendable {
    public let localIdentifier: String
    public let filename: String
    public let file: URL
    public let sha256: String
    public let mtimeSeconds: Int

    public init(localIdentifier: String, filename: String, file: URL, sha256: String, mtimeSeconds: Int) {
        self.localIdentifier = localIdentifier
        self.filename = filename
        self.file = file
        self.sha256 = sha256
        self.mtimeSeconds = mtimeSeconds
    }
}

/// PhotoKit originals for the background upload. The asset stays in Photos.
@MainActor
public enum OriginalExportSession {
    public static func export(localIdentifier: String, scratch: URL) async throws -> [ExportedOriginal] {
        let assets = PHAsset.fetchAssets(withLocalIdentifiers: [localIdentifier], options: nil)
        guard let asset = assets.firstObject else {
            throw UploadFailure.noOriginal
        }
        let items = try OriginalResources.forAsset(asset)
        if items.isEmpty {
            throw UploadFailure.noOriginal
        }
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
        let when = asset.modificationDate ?? asset.creationDate ?? Date()
        let mtime = Int(when.timeIntervalSince1970)
        var exported: [ExportedOriginal] = []
        do {
            for item in items {
                let dest = scratch.appendingPathComponent(UUID().uuidString, isDirectory: false)
                try await OriginalResources.write(item.resource, to: dest)
                var stamped = dest
                var values = URLResourceValues()
                values.contentModificationDate = when
                try stamped.setResourceValues(values)
                let sha = try Sha256File.hex(dest)
                let name = try StoredName.require(item.uploadFilename)
                exported.append(
                    ExportedOriginal(
                        localIdentifier: localIdentifier,
                        filename: name,
                        file: dest,
                        sha256: sha,
                        mtimeSeconds: mtime
                    )
                )
            }
        } catch {
            for done in exported {
                try? BackgroundScratch.remove(scratch: scratch, file: done.file)
            }
            throw error
        }
        return exported
    }
}

@MainActor
public enum PhotoLibraryBackup {
    public static func authorizationFailure() -> String? {
        switch PHPhotoLibrary.authorizationStatus(for: .readWrite) {
        case .authorized, .limited:
            return nil
        default:
            return "Photos access is off. Albumen has not read the camera roll."
        }
    }

    public static func assets(createdAfter timestamp: Int?) -> [LibraryAsset] {
        let options = PHFetchOptions()
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: true)]
        if let timestamp {
            let date = Date(timeIntervalSince1970: TimeInterval(timestamp))
            options.predicate = NSPredicate(format: "creationDate > %@", date as NSDate)
        }
        let result = PHAsset.fetchAssets(with: options)
        var assets: [LibraryAsset] = []
        assets.reserveCapacity(result.count)
        result.enumerateObjects { asset, _, _ in
            let created = Int(asset.creationDate?.timeIntervalSince1970 ?? 0)
            assets.append(LibraryAsset(localIdentifier: asset.localIdentifier, creationTimestamp: created))
        }
        return assets
    }

    /// Copy queued originals and hand the files to the background session.
    /// `includeExisting` is the person's own request, so rows waiting on a full
    /// original are tried again. An automatic pass leaves those rows queued.
    public static func run(directory: URL, accessGroup: String?, includeExisting: Bool) async -> String {
        do {
            if includeExisting {
                _ = try BackupReportStore.update(directory: directory) { report in
                    report.verifiedThisRun = 0
                    report.alreadyVerified = 0
                    report.failures = []
                }
            }
            let config = try ReceiverEndpointStore.load(directory: directory)
            guard let endpoint = URL(string: config.endpoint),
                  endpoint.scheme == "http" || endpoint.scheme == "https",
                  !config.user.isEmpty
            else {
                return try record(
                    directory,
                    UploadFailure.transport("the receiver address or user is missing").surfaced(identifier: "Backup")
                )
            }
            guard try ReceiverPassword.read(accessGroup: accessGroup) != nil else {
                return try record(directory, UploadFailure.missingPassword.surfaced(identifier: "Backup"))
            }
            if let blocked = authorizationFailure() {
                return try record(directory, blocked)
            }

            var queue = try UploadQueueStore.load(directory: directory)
            let fresh = assets(createdAfter: includeExisting ? nil : queue.lastCreationTimestamp)
            for asset in fresh {
                queue.enqueue(asset.localIdentifier)
                queue.lastCreationTimestamp = max(queue.lastCreationTimestamp ?? asset.creationTimestamp, asset.creationTimestamp)
            }
            if includeExisting {
                queue.retryOnUserRequest()
            }
            try UploadQueueStore.save(queue, directory: directory)

            let inflight = Set(try PendingUploadStore.load(directory: directory).map(\.localIdentifier))
            let waiting = queue.pending.filter { identifier in
                !inflight.contains(identifier) && (includeExisting || queue.derivativeMessages[identifier] == nil)
            }
            if waiting.isEmpty && fresh.isEmpty {
                return try surfaceDerivatives(directory, queue)
            }

            let session = BackgroundUploadController.shared.open(
                user: config.user,
                accessGroup: accessGroup,
                directory: directory
            )
            let scratch = directory.appendingPathComponent("scratch", isDirectory: true)
            try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
            for identifier in waiting {
                do {
                    let exported = try await OriginalExportSession.export(localIdentifier: identifier, scratch: scratch)
                    for item in exported {
                        try session.enqueue(
                            file: item.file,
                            filename: item.filename,
                            clientSha256: item.sha256,
                            localIdentifier: identifier,
                            mtimeSeconds: item.mtimeSeconds,
                            endpoint: endpoint
                        )
                    }
                    queue.derivativeMessages[identifier] = nil
                } catch let failure as UploadFailure {
                    let message = failure.surfaced(identifier: identifier)
                    if case .derivativeOnly = failure {
                        queue.noteDerivative(identifier: identifier, message: message)
                    }
                    _ = try BackupReportStore.update(directory: directory) { report in
                        report.failures.append(message)
                    }
                } catch {
                    _ = try BackupReportStore.update(directory: directory) { report in
                        report.failures.append("\(identifier) was not copied: \(error)")
                    }
                }
            }
            try UploadQueueStore.save(queue, directory: directory)
            return try surfaceDerivatives(directory, queue)
        } catch {
            return (try? record(directory, "The backup did not start: \(error)"))
                ?? "The backup did not start: \(error)\nVerified 0 this run. Already verified 0. Failures 1."
        }
    }

    private static func surfaceDerivatives(_ directory: URL, _ queue: UploadQueue) throws -> String {
        let report = try BackupReportStore.update(directory: directory) { report in
            for message in queue.derivativeMessages.values.sorted() where !report.failures.contains(message) {
                report.failures.append(message)
            }
        }
        return report.text
    }

    private static func record(_ directory: URL, _ message: String) throws -> String {
        let report = try BackupReportStore.update(directory: directory) { report in
            report.failures.append(message)
        }
        return report.text
    }
}
