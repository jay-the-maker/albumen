// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test
import java.io.File

class UploadCheckTest {
    @Test
    fun createdCollisionAndIdenticalRetry() {
        assertEquals(
            UploadVerdict.Verified("ab"),
            UploadCheck.verdict("Ab", 201, "ab"),
        )
        assertEquals(
            UploadVerdict.Verified("ab"),
            UploadCheck.verdict("ab", 409, "ab"),
        )
    }

    @Test
    fun mismatchIsAConflictAndAMissingHeaderIsNotSuccess() {
        assertEquals(
            UploadVerdict.Conflict("aa", "bb"),
            UploadCheck.verdict("aa", 201, "bb"),
        )
        assertEquals(
            UploadVerdict.Conflict("aa", "bb"),
            UploadCheck.verdict("aa", 409, "bb"),
        )
        assertEquals(
            UploadVerdict.MissingServerHash,
            UploadCheck.verdict("aa", 201, null),
        )
        assertEquals(
            UploadVerdict.Rejected(500),
            UploadCheck.verdict("aa", 500, "aa"),
        )
    }

    @Test
    fun digestMatchesTheKnownVector() {
        assertEquals(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            Sha256.hex("abc".toByteArray(Charsets.UTF_8)),
        )
    }

    @Test
    fun workManagerInputOmitsTheReceiverPassword() {
        val secret = "receiver-password-not-for-workmanager"
        val data = backupInput("https://albumen.example", "camera", secret)
        assertEquals("https://albumen.example", data.getString(CameraRollBackupWorker.KEY_ENDPOINT))
        assertEquals("camera", data.getString(CameraRollBackupWorker.KEY_USER))
        assertNull(data.getString("password"))
        for ((key, value) in data.keyValueMap) {
            assertFalse(key.contains(secret))
            assertFalse(value.toString().contains(secret))
        }
        val worker = File("src/main/kotlin/photos/albumen/backup/CameraRollBackupWorker.kt").readText()
        val input = File("src/main/kotlin/photos/albumen/backup/BackupWorkInput.kt").readText()
        assertFalse(worker.contains("KEY_PASSWORD"))
        assertFalse(worker.contains("workDataOf"))
        assertFalse(input.contains("KEY_PASSWORD"))
        assertFalse(input.contains("to password"))
        assertFalse(input.contains("\"$secret\""))
    }

    @Test
    fun sourceMtimeHeaderIsUnixSeconds() {
        assertEquals("1000000000", sourceMtimeSeconds(1_000_000_000_000L))
    }

    @Test
    fun storedNameKeepsTheCameraFilename() {
        assertEquals("IMG_0001.JPG", StoredName.require("/DCIM/IMG_0001.JPG"))
        assertThrows(IllegalArgumentException::class.java) {
            StoredName.require("../IMG_0001.JPG")
        }
    }
}
