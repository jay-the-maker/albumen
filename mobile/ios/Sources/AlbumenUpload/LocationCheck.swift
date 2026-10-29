// SPDX-License-Identifier: Apache-2.0

/// Issue #25: PhotoKit can hand back an original whose GPS was removed (for
/// example after "Location: Off" sharing) while Photos still reports a
/// location for the asset. Those bytes are not the camera original, so they
/// are refused the same way a derivative is and stay queued.
public enum LocationCheck {
    public static let strippedDetail = "GPS is missing from the original but Photos has a location"

    public static func refusal(assetHasLocation: Bool, originalHasGPS: Bool) -> UploadFailure? {
        if assetHasLocation && !originalHasGPS {
            return .derivativeOnly(strippedDetail)
        }
        return nil
    }
}
