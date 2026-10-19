// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.Base64

/**
 * A one-pixel JPEG with a synthetic GPS EXIF block: 1°2'3" N, 4°5'6" E.
 * The pixel and the coordinates are generated. This is not a photograph.
 */
object SyntheticGpsJpeg {
    const val LAT_REF = 'N'
    const val LON_REF = 'E'
    const val DISPLAY_NAME = "albumen-synthetic-gps.jpg"

    fun bytes(): ByteArray {
        val base = Base64.getDecoder().decode(BASE_JPEG.replace("\n", ""))
        require(base.size > 2 && base[0] == 0xFF.toByte() && base[1] == 0xD8.toByte())
        val exif = app1(tiff())
        val out = ByteArray(2 + exif.size + base.size - 2)
        out[0] = 0xFF.toByte()
        out[1] = 0xD8.toByte()
        exif.copyInto(out, destinationOffset = 2)
        base.copyInto(out, destinationOffset = 2 + exif.size, startIndex = 2, endIndex = base.size)
        return out
    }

    /**
     * The first Exif APP1 segment, marker included.
     * For [bytes] that segment is the synthetic GPS block (1°2'3" N, 4°5'6" E).
     */
    fun exifApp1(jpeg: ByteArray): ByteArray {
        require(jpeg.size >= 4 && jpeg[0] == 0xFF.toByte() && jpeg[1] == 0xD8.toByte()) {
            "not a JPEG"
        }
        var index = 2
        while (index + 4 <= jpeg.size) {
            if (jpeg[index] != 0xFF.toByte()) {
                throw IllegalArgumentException("JPEG marker expected at $index")
            }
            val marker = jpeg[index + 1].toInt() and 0xFF
            if (marker == 0xD8 || marker == 0x01 || marker in 0xD0..0xD7) {
                index += 2
                continue
            }
            if (marker == 0xD9 || marker == 0xDA) {
                break
            }
            val length = ((jpeg[index + 2].toInt() and 0xFF) shl 8) or (jpeg[index + 3].toInt() and 0xFF)
            if (length < 2 || index + 2 + length > jpeg.size) {
                throw IllegalArgumentException("JPEG segment length $length at $index")
            }
            if (marker == 0xE1 && length >= 8 && isExifHeader(jpeg, index + 4)) {
                return jpeg.copyOfRange(index, index + 2 + length)
            }
            index += 2 + length
        }
        throw IllegalArgumentException("JPEG has no Exif APP1 segment")
    }

    private fun isExifHeader(jpeg: ByteArray, offset: Int): Boolean {
        val header = byteArrayOf('E'.code.toByte(), 'x'.code.toByte(), 'i'.code.toByte(), 'f'.code.toByte(), 0, 0)
        if (offset + header.size > jpeg.size) {
            return false
        }
        for (i in header.indices) {
            if (jpeg[offset + i] != header[i]) {
                return false
            }
        }
        return true
    }

    private fun tiff(): ByteArray {
        val tiff = ByteBuffer.allocate(128).order(ByteOrder.LITTLE_ENDIAN)
        tiff.put("II".toByteArray(Charsets.US_ASCII))
        tiff.putShort(42)
        tiff.putInt(8)
        tiff.putShort(1)
        tiff.putShort(0x8825.toShort())
        tiff.putShort(4)
        tiff.putInt(1)
        tiff.putInt(26)
        tiff.putInt(0)
        tiff.putShort(4)
        putAsciiRef(tiff, tag = 1, ref = LAT_REF)
        putRationalPointer(tiff, tag = 2, offset = 80)
        putAsciiRef(tiff, tag = 3, ref = LON_REF)
        putRationalPointer(tiff, tag = 4, offset = 104)
        tiff.putInt(0)
        putRational(tiff, 1, 1)
        putRational(tiff, 2, 1)
        putRational(tiff, 3, 1)
        putRational(tiff, 4, 1)
        putRational(tiff, 5, 1)
        putRational(tiff, 6, 1)
        check(tiff.position() == 128) { "synthetic TIFF is ${tiff.position()} bytes" }
        return tiff.array()
    }

    private fun putAsciiRef(tiff: ByteBuffer, tag: Int, ref: Char) {
        tiff.putShort(tag.toShort())
        tiff.putShort(2)
        tiff.putInt(2)
        tiff.put(ref.code.toByte())
        tiff.put(0)
        tiff.put(0)
        tiff.put(0)
    }

    private fun putRationalPointer(tiff: ByteBuffer, tag: Int, offset: Int) {
        tiff.putShort(tag.toShort())
        tiff.putShort(5)
        tiff.putInt(3)
        tiff.putInt(offset)
    }

    private fun putRational(tiff: ByteBuffer, numerator: Int, denominator: Int) {
        tiff.putInt(numerator)
        tiff.putInt(denominator)
    }

    private fun app1(tiff: ByteArray): ByteArray {
        val header = byteArrayOf('E'.code.toByte(), 'x'.code.toByte(), 'i'.code.toByte(), 'f'.code.toByte(), 0, 0)
        val length = 2 + header.size + tiff.size
        val out = ByteBuffer.allocate(2 + length).order(ByteOrder.BIG_ENDIAN)
        out.put(0xFF.toByte())
        out.put(0xE1.toByte())
        out.putShort(length.toShort())
        out.put(header)
        out.put(tiff)
        return out.array()
    }

    private const val BASE_JPEG =
        """/9j/4AAQSkZJRgABAgAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0a
HBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIy
MjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAABAAEDASIA
AhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQA
AAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3
ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWm
p6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEA
AwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSEx
BhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElK
U1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3
uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDnKKKK
948Q/9k="""
}
