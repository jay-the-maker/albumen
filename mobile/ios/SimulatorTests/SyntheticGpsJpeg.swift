// SPDX-License-Identifier: Apache-2.0

import Foundation

/// A one-pixel JPEG with a synthetic GPS EXIF block: 1°2'3" N, 4°5'6" E.
/// The pixel and the coordinates are generated. This is not a photograph.
enum SyntheticGpsJpeg {
    static func bytes() -> Data {
        guard let base = Data(base64Encoded: baseJPEG, options: .ignoreUnknownCharacters) else {
            preconditionFailure("synthetic JPEG base is not base64")
        }
        precondition(base.count > 2 && base[0] == 0xFF && base[1] == 0xD8)
        let exif = app1(tiff())
        var out = Data([0xFF, 0xD8])
        out.append(exif)
        out.append(base.dropFirst(2))
        return out
    }

    /// The first Exif APP1 segment, marker included.
    static func exifApp1(_ jpeg: Data) -> Data {
        precondition(jpeg.count >= 4 && jpeg[0] == 0xFF && jpeg[1] == 0xD8, "not a JPEG")
        var index = 2
        while index + 4 <= jpeg.count {
            precondition(jpeg[index] == 0xFF, "JPEG marker expected")
            let marker = Int(jpeg[index + 1])
            if marker == 0xD8 || marker == 0x01 || (0xD0...0xD7).contains(marker) {
                index += 2
                continue
            }
            if marker == 0xD9 || marker == 0xDA {
                break
            }
            let length = (Int(jpeg[index + 2]) << 8) | Int(jpeg[index + 3])
            precondition(length >= 2 && index + 2 + length <= jpeg.count, "JPEG segment length")
            if marker == 0xE1 && length >= 8 && isExifHeader(jpeg, index + 4) {
                return jpeg.subdata(in: index..<(index + 2 + length))
            }
            index += 2 + length
        }
        preconditionFailure("JPEG has no Exif APP1 segment")
    }

    private static func isExifHeader(_ jpeg: Data, _ offset: Int) -> Bool {
        let header: [UInt8] = [0x45, 0x78, 0x69, 0x66, 0, 0]
        guard offset + header.count <= jpeg.count else {
            return false
        }
        return zip(header, jpeg[offset..<(offset + header.count)]).allSatisfy { $0 == $1 }
    }

    private static func tiff() -> Data {
        var bytes = Data()
        func put(_ value: UInt8) { bytes.append(value) }
        func put16(_ value: UInt16) {
            put(UInt8(value & 0xFF))
            put(UInt8(value >> 8))
        }
        func put32(_ value: UInt32) {
            put(UInt8(value & 0xFF))
            put(UInt8((value >> 8) & 0xFF))
            put(UInt8((value >> 16) & 0xFF))
            put(UInt8((value >> 24) & 0xFF))
        }
        func asciiRef(tag: UInt16, ref: UInt8) {
            put16(tag)
            put16(2)
            put32(2)
            put(ref)
            put(0)
            put(0)
            put(0)
        }
        func rationalPointer(tag: UInt16, offset: UInt32) {
            put16(tag)
            put16(5)
            put32(3)
            put32(offset)
        }
        func rational(_ numerator: UInt32, _ denominator: UInt32) {
            put32(numerator)
            put32(denominator)
        }
        bytes.append(contentsOf: [0x49, 0x49])
        put16(42)
        put32(8)
        put16(1)
        put16(0x8825)
        put16(4)
        put32(1)
        put32(26)
        put32(0)
        put16(4)
        asciiRef(tag: 1, ref: UInt8(ascii: "N"))
        rationalPointer(tag: 2, offset: 80)
        asciiRef(tag: 3, ref: UInt8(ascii: "E"))
        rationalPointer(tag: 4, offset: 104)
        put32(0)
        rational(1, 1)
        rational(2, 1)
        rational(3, 1)
        rational(4, 1)
        rational(5, 1)
        rational(6, 1)
        precondition(bytes.count == 128, "synthetic TIFF is \(bytes.count) bytes")
        return bytes
    }

    private static func app1(_ tiff: Data) -> Data {
        let header: [UInt8] = [0x45, 0x78, 0x69, 0x66, 0, 0]
        let length = 2 + header.count + tiff.count
        var out = Data([0xFF, 0xE1])
        out.append(UInt8((length >> 8) & 0xFF))
        out.append(UInt8(length & 0xFF))
        out.append(contentsOf: header)
        out.append(tiff)
        return out
    }

    private static let baseJPEG = """
    /9j/4AAQSkZJRgABAgAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0a\
    HBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIy\
    MjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAABAAEDASIA\
    AhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQA\
    AAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3\
    ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWm\
    p6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEA\
    AwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSEx\
    BhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElK\
    U1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3\
    uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDnKKKK\
    948Q/9k=
    """
}
