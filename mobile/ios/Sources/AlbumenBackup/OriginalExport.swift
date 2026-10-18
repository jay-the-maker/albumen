// SPDX-License-Identifier: Apache-2.0

import Foundation
import os
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

private let originalsLog = Logger(subsystem: "photos.albumen.backup", category: "originals")

/// PhotoKit originals. This does not call `PHImageManager.requestImage`, which
/// returns a rendered preview.
public enum OriginalResources {
    public static func forAsset(_ asset: PHAsset) throws -> [OriginalItem] {
        let resources = PHAssetResource.assetResources(for: asset)
        if resources.isEmpty {
            throw UploadFailure.noOriginal
        }
        let remote = originalIsRemote(asset, resources: resources)
        switch asset.mediaType {
        case .image:
            return try stillItems(resources, isRemote: remote)
        case .video:
            return try videoItems(resources, isRemote: remote)
        default:
            throw UploadFailure.noOriginal
        }
    }

    /// Write the resource's original bytes. Network access stays on so an
    /// iCloud-optimized asset downloads the full original. The destination
    /// file must not exist yet.
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

    private static func stillItems(
        _ resources: [PHAssetResource],
        isRemote: Bool
    ) throws -> [OriginalItem] {
        let present = resources.compactMap { kind($0.type) }
        let stillKind = try require(
            OriginalPolicy.choose(role: .still, present: present, isRemote: isRemote)
        )
        guard let still = resources.first(where: { kind($0.type) == stillKind }) else {
            throw UploadFailure.noOriginal
        }
        var items = [OriginalItem(uploadFilename: still.originalFilename, resource: still)]
        if let alternate = resources.first(where: { $0.type == .alternatePhoto }),
           alternate.originalFilename != still.originalFilename
        {
            items.append(OriginalItem(uploadFilename: alternate.originalFilename, resource: alternate))
        }
        try appendPaired(resources, isRemote: isRemote, to: &items)
        if let adjustment = resources.first(where: { $0.type == .adjustmentData }) {
            items.append(
                OriginalItem(
                    uploadFilename: "\(still.originalFilename).adjustment",
                    resource: adjustment
                )
            )
        }
        return items
    }

    private static func videoItems(
        _ resources: [PHAssetResource],
        isRemote: Bool
    ) throws -> [OriginalItem] {
        let present = resources.compactMap { kind($0.type) }
        let videoKind = try require(
            OriginalPolicy.choose(role: .video, present: present, isRemote: isRemote)
        )
        guard let video = resources.first(where: { kind($0.type) == videoKind }) else {
            throw UploadFailure.noOriginal
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
    }

    private static func appendPaired(
        _ resources: [PHAssetResource],
        isRemote: Bool,
        to items: inout [OriginalItem]
    ) throws {
        let present = resources.compactMap { kind($0.type) }.filter {
            $0 == .fullSizePairedVideo || $0 == .pairedVideo
        }
        if present.isEmpty {
            return
        }
        let pairedKind = try require(
            OriginalPolicy.choose(role: .pairedVideo, present: present, isRemote: isRemote)
        )
        guard let paired = resources.first(where: { kind($0.type) == pairedKind }) else {
            throw UploadFailure.noOriginal
        }
        items.append(OriginalItem(uploadFilename: paired.originalFilename, resource: paired))
    }

    private static func require(_ decision: OriginalDecision) throws -> ResourceKind {
        switch decision {
        case .use(let kind):
            return kind
        case .derivativeOnly(let present):
            let names = present.map(\.rawValue).joined(separator: ",")
            originalsLog.error(
                "refusing a derivative; the full iCloud original is not available: \(names, privacy: .public)"
            )
            throw UploadFailure.derivativeOnly(names)
        }
    }

    /// An iCloud shared asset, or a full-size resource PhotoKit says is not
    /// local, still needs the network fetch. A missing `locallyAvailable` key
    /// is treated as local so an ordinary library asset is not refused.
    static func originalIsRemote(_ asset: PHAsset, resources: [PHAssetResource]) -> Bool {
        if asset.sourceType.contains(.typeCloudShared) || !asset.sourceType.contains(.typeUserLibrary) {
            return true
        }
        for resource in resources where locallyAvailable(resource) == false {
            return true
        }
        return false
    }

    private static func locallyAvailable(_ resource: PHAssetResource) -> Bool? {
        let selector = NSSelectorFromString("locallyAvailable")
        guard resource.responds(to: selector) else {
            return nil
        }
        return resource.value(forKey: "locallyAvailable") as? Bool
    }

    private static func kind(_ type: PHAssetResourceType) -> ResourceKind? {
        switch type {
        case .fullSizePhoto: return .fullSizePhoto
        case .adjustmentBasePhoto: return .adjustmentBasePhoto
        case .photo: return .photo
        case .alternatePhoto: return .alternatePhoto
        case .fullSizePairedVideo: return .fullSizePairedVideo
        case .pairedVideo: return .pairedVideo
        case .fullSizeVideo: return .fullSizeVideo
        case .adjustmentBaseVideo: return .adjustmentBaseVideo
        case .video: return .video
        case .adjustmentData: return .adjustmentData
        default: return nil
        }
    }
}
