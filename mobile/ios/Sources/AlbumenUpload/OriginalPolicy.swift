// SPDX-License-Identifier: Apache-2.0

/// Which original a camera-roll export may upload.
///
/// A full-size PhotoKit type is the original. `.photo`, `.video`, and
/// `.pairedVideo` are the original only when that is the only type PhotoKit
/// has and the asset is local. On an iCloud-optimized asset those three are
/// derivatives, and the export fails instead of uploading one.
public enum ResourceKind: String, Equatable, Sendable {
    case fullSizePhoto
    case adjustmentBasePhoto
    case photo
    case alternatePhoto
    case fullSizePairedVideo
    case pairedVideo
    case fullSizeVideo
    case adjustmentBaseVideo
    case video
    case adjustmentData
}

public enum ExportRole: Equatable, Sendable {
    case still
    case pairedVideo
    case video
}

public enum OriginalDecision: Equatable, Sendable {
    case use(ResourceKind)
    case derivativeOnly(present: [ResourceKind])
}

public enum OriginalPolicy {
    public static func choose(
        role: ExportRole,
        present: [ResourceKind],
        isRemote: Bool
    ) -> OriginalDecision {
        let preferred: [ResourceKind]
        let localOriginal: ResourceKind
        switch role {
        case .still:
            preferred = [.fullSizePhoto, .adjustmentBasePhoto]
            localOriginal = .photo
        case .pairedVideo:
            preferred = [.fullSizePairedVideo]
            localOriginal = .pairedVideo
        case .video:
            preferred = [.fullSizeVideo, .adjustmentBaseVideo]
            localOriginal = .video
        }
        for kind in preferred where present.contains(kind) {
            return .use(kind)
        }
        if present.contains(localOriginal) && !isRemote {
            return .use(localOriginal)
        }
        return .derivativeOnly(present: present)
    }
}
