// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder

class SyntheticGpsJpegTest {
    @Test
    fun gpsExifSurvivesTheCacheCopy() {
        val jpeg = SyntheticGpsJpeg.bytes()
        assertEquals(0xFF.toByte(), jpeg[0])
        assertEquals(0xD8.toByte(), jpeg[1])
        assertTrue(String(jpeg, Charsets.ISO_8859_1).contains("Exif"))
        val rationals = ByteBuffer.allocate(24).order(ByteOrder.LITTLE_ENDIAN)
        listOf(1 to 1, 2 to 1, 3 to 1).forEach { (numerator, denominator) ->
            rationals.putInt(numerator)
            rationals.putInt(denominator)
        }
        assertTrue(contains(jpeg, rationals.array()))
        assertTrue(contains(jpeg, byteArrayOf(SyntheticGpsJpeg.LAT_REF.code.toByte(), 0)))
        assertTrue(contains(jpeg, byteArrayOf(SyntheticGpsJpeg.LON_REF.code.toByte(), 0)))
        val frame = sofSize(jpeg)
        assertEquals(1, frame.first)
        assertEquals(1, frame.second)

        val dir = File("build/synthetic-gps").apply { mkdirs() }
        val spool = copyToCache(jpeg.inputStream(), SyntheticGpsJpeg.DISPLAY_NAME, dir)
        try {
            assertEquals(Sha256.hex(jpeg), spool.sha256)
            assertArrayEquals(jpeg, spool.file.readBytes())
            assertEquals(SyntheticGpsJpeg.DISPLAY_NAME, spool.filename)
        } finally {
            spool.file.delete()
        }
    }

    /** SOF height and width. The synthetic frame is one pixel. */
    private fun sofSize(jpeg: ByteArray): Pair<Int, Int> {
        var index = 2
        while (index + 8 < jpeg.size) {
            if (jpeg[index] != 0xFF.toByte()) {
                index++
                continue
            }
            val marker = jpeg[index + 1].toInt() and 0xFF
            if (marker == 0xC0 || marker == 0xC2) {
                val height = ((jpeg[index + 5].toInt() and 0xFF) shl 8) or (jpeg[index + 6].toInt() and 0xFF)
                val width = ((jpeg[index + 7].toInt() and 0xFF) shl 8) or (jpeg[index + 8].toInt() and 0xFF)
                return height to width
            }
            if (marker == 0xD8 || marker == 0xD9 || marker == 0x01 || (marker in 0xD0..0xD7)) {
                index += 2
                continue
            }
            val length = ((jpeg[index + 2].toInt() and 0xFF) shl 8) or (jpeg[index + 3].toInt() and 0xFF)
            index += 2 + length
        }
        throw AssertionError("JPEG has no SOF marker")
    }

    private fun contains(haystack: ByteArray, needle: ByteArray): Boolean {
        if (needle.isEmpty() || needle.size > haystack.size) {
            return false
        }
        for (start in 0..haystack.size - needle.size) {
            var matched = true
            for (index in needle.indices) {
                if (haystack[start + index] != needle[index]) {
                    matched = false
                    break
                }
            }
            if (matched) {
                return true
            }
        }
        return false
    }
}
