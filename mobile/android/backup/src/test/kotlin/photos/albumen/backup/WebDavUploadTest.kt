// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

class WebDavUploadTest {
    @Test
    fun putSendsTheBytesTheMtimeAndMatchesTheServerHash() {
        val jpeg = SyntheticGpsJpeg.bytes()
        val dir = File("build/webdav-put").apply { mkdirs() }
        val file = File(dir, SyntheticGpsJpeg.DISPLAY_NAME)
        file.writeBytes(jpeg)
        file.setLastModified(5_000L)
        val sha = Sha256.hex(jpeg)
        LoopbackWebDav { headers, body ->
            assertTrue(headers.contains("X-Albumen-Mtime: 1700000000"))
            assertEquals(sha, Sha256.hex(body))
            LoopbackWebDav.Reply(201, sha, "Created")
        }.use { server ->
            val verdict = WebDavUploader(server.endpoint(), "camera", "secret")
                .put(file, file.name, sha, 1_700_000_000L)
            assertEquals(UploadVerdict.Verified(sha), verdict)
            assertEquals(1, server.bodies.size)
        }
    }

    @Test
    fun encodedFilenameKeepsASpaceOutOfThePath() {
        assertEquals("IMG%200001.JPG", encodePathSegment("IMG 0001.JPG"))
        assertTrue(endpointUrl("http://127.0.0.1:9", "IMG 0001.JPG").path.endsWith("IMG%200001.JPG"))
    }
}
