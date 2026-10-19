// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import javax.imageio.ImageIO

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

        val image = ImageIO.read(jpeg.inputStream())
        assertNotNull(image)
        assertEquals(1, image.width)
        assertEquals(1, image.height)

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
