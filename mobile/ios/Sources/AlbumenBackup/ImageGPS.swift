// SPDX-License-Identifier: Apache-2.0

import Foundation
import ImageIO

/// Reads only metadata. The exported file is not decoded or rewritten.
enum ImageGPS {
    static func present(_ file: URL) -> Bool {
        guard let source = CGImageSourceCreateWithURL(file as CFURL, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let gps = properties[kCGImagePropertyGPSDictionary] as? [CFString: Any]
        else {
            return false
        }
        return gps[kCGImagePropertyGPSLatitude] != nil && gps[kCGImagePropertyGPSLongitude] != nil
    }
}
