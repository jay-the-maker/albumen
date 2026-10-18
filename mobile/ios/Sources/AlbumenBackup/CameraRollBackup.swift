// SPDX-License-Identifier: Apache-2.0

import Foundation
@preconcurrency import Photos

import AlbumenUpload

/// Back up one Photos asset. The asset stays in the camera roll. Scratch files
/// are the only things this deletes, and only after the upload attempt returns.
public enum CameraRollBackup {
    public static func backup(
        asset: PHAsset,
        scratch: URL,
        uploader: WebDavUploader
    ) async throws -> [VerifiedUpload] {
        let items = try OriginalResources.forAsset(asset)
        if items.isEmpty {
            throw UploadFailure.noOriginal
        }
        let names = items.map(\.uploadFilename)
        if Set(names).count != names.count {
            throw UploadFailure.badFilename("two originals share a filename")
        }
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
        var verified: [VerifiedUpload] = []
        for item in items {
            let dest = scratch.appendingPathComponent(UUID().uuidString, isDirectory: false)
            do {
                try await OriginalResources.write(item.resource, to: dest)
                let verdict = try await uploader.putOriginal(filename: item.uploadFilename, file: dest)
                let sha256 = try UploadCheck.requireVerified(verdict)
                verified.append(VerifiedUpload(filename: item.uploadFilename, sha256: sha256))
            } catch {
                try? FileManager.default.removeItem(at: dest)
                throw error
            }
            try? FileManager.default.removeItem(at: dest)
        }
        return verified
    }
}
