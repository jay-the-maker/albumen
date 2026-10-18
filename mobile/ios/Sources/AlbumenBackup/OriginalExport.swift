// SPDX-License-Identifier: Apache-2.0

import Foundation
@preconcurrency import Photos

import AlbumenUpload

public struct OriginalItem {
    public let uploadFilename: String
    public let resource: PHAssetResource

    public init(uploadFilename: String, resource: PHAssetResource) {
        self.uploadFilename = uploadFilename
        self.resource = resource
    }
}

/// PhotoKit originals. This does not call `PHImageManager.requestImage`, which
/// returns a rendered preview.
public enum OriginalResources {
    public static func forAsset(_ asset: PHAsset) -> [OriginalItem] {
        let resources = PHAssetResource.assetResources(for: asset)
        switch asset.mediaType {
        case .image:
            guard let still = prefer(resources, [.fullSizePhoto, .adjustmentBasePhoto, .photo]) else {
                return []
            }
            var items = [OriginalItem(uploadFilename: still.originalFilename, resource: still)]
            if let alternate = resources.first(where: { $0.type == .alternatePhoto }),
               alternate.originalFilename != still.originalFilename
            {
                items.append(OriginalItem(uploadFilename: alternate.originalFilename, resource: alternate))
            }
            if let paired = prefer(resources, [.fullSizePairedVideo, .pairedVideo]) {
                items.append(OriginalItem(uploadFilename: paired.originalFilename, resource: paired))
            }
            if let adjustment = resources.first(where: { $0.type == .adjustmentData }) {
                items.append(
                    OriginalItem(
                        uploadFilename: "\(still.originalFilename).adjustment",
                        resource: adjustment
                    )
                )
            }
            return items
        case .video:
            guard let video = prefer(resources, [.fullSizeVideo, .adjustmentBaseVideo, .video]) else {
                return []
            }
            var items = [OriginalItem(uploadFilename: video.originalFilename, resource: video)]
            if let adjustment = resources.first(where: { $0.type == .adjustmentData }) {
                items.append(
                    OriginalItem(
                        uploadFilename: "\(video.originalFilename).adjustment",
                        resource: adjustment
                    )
                )
            }
            return items
        default:
            return []
        }
    }

    /// Write the resource's original bytes. The destination file must not exist yet.
    public static func write(_ resource: PHAssetResource, to fileURL: URL) async throws {
        let options = PHAssetResourceRequestOptions()
        options.isNetworkAccessAllowed = true
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            PHAssetResourceManager.default().writeData(
                for: resource,
                toFile: fileURL,
                options: options
            ) { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }
    }

    private static func prefer(
        _ resources: [PHAssetResource],
        _ types: [PHAssetResourceType]
    ) -> PHAssetResource? {
        for type in types {
            if let found = resources.first(where: { $0.type == type }) {
                return found
            }
        }
        return nil
    }
}
