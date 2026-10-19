// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.app.Application
import android.Manifest
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class BackupSessionTest {
    @get:Rule
    val folder = TemporaryFolder()

    @Test
    fun mismatchRetriesAndNamesBothHashes() {
        val secret = "receiver-password-sentinel"
        val item = item("IMG.JPG")
        var calls = 0
        val seen = run(
            items = listOf(item),
            password = secret,
            uploader = { _, _, sha, mtime ->
                calls++
                assertEquals(1_700_000_000L, mtime)
                UploadVerdict.Conflict(sha, "deadbeef")
            },
        )
        assertEquals(3, calls)
        assertEquals(BackupDisposition.RETRY, seen.disposition)
        val text = seen.texts.last()
        assertTrue(text.contains("Client"))
        assertTrue(text.contains("deadbeef"))
        assertTrue(text.contains("SHA-256 mismatch"))
        assertFalse(text.contains(secret))
        assertTrue(text.contains("Failures 1"))
        assertTrue(seen.texts.all { it.isNotBlank() })
    }

    @Test
    fun verifiedItemIsSkippedOnTheNextPass() {
        val item = item("IMG.JPG")
        val bytes = "abc".toByteArray()
        var calls = 0
        val first = run(
            items = listOf(item),
            uploader = { _, _, sha, _ ->
                calls++
                UploadVerdict.Verified(sha)
            },
            bytes = bytes,
        )
        assertEquals(BackupDisposition.SUCCESS, first.disposition)
        assertEquals(1, calls)
        val second = run(
            items = listOf(item),
            progress = first.progress,
            uploader = { _, _, _, _ ->
                calls++
                UploadVerdict.Verified("unused")
            },
            bytes = bytes,
        )
        assertEquals(1, calls)
        assertEquals(BackupDisposition.SUCCESS, second.disposition)
        assertTrue(second.texts.last().contains("Already verified 1"))
    }

    @Test
    fun authFailureStopsWithoutARetryLoop() {
        var calls = 0
        val seen = run(
            items = listOf(item("IMG.JPG"), item("IMG2.JPG", id = 2)),
            uploader = { _, _, _, _ ->
                calls++
                UploadVerdict.Rejected(401)
            },
        )
        assertEquals(1, calls)
        assertEquals(BackupDisposition.FAILURE, seen.disposition)
        assertTrue(seen.texts.last().contains("HTTP 401"))
    }

    @Test
    fun missingLocationIsAVisibleStop() {
        var calls = 0
        val seen = run(
            items = listOf(item("IMG.JPG")),
            permission = { CameraRoll.LOCATION_BLOCK },
            uploader = { _, _, _, _ ->
                calls++
                UploadVerdict.Verified("nope")
            },
        )
        assertEquals(0, calls)
        assertEquals(BackupDisposition.FAILURE, seen.disposition)
        assertTrue(seen.texts.last().contains("GPS"))
    }

    @Test
    fun selectedPhotosIsAVisibleStopBeforeAnyRead() {
        var calls = 0
        val seen = run(
            items = listOf(item("IMG.JPG")),
            permission = { CameraRoll.SELECTED_ONLY_BLOCK },
            uploader = { _, _, _, _ ->
                calls++
                UploadVerdict.Verified("nope")
            },
        )
        assertEquals(0, calls)
        assertEquals(BackupDisposition.FAILURE, seen.disposition)
        assertTrue(seen.texts.last().contains("Allow all"))
        assertTrue(seen.texts.last().isNotBlank())
    }

    @Test
    fun accessDroppingToSelectedPhotosMidPassStopsBeforeTheNextItem() {
        var checks = 0
        var calls = 0
        val seen = run(
            items = listOf(item("A.JPG", 1), item("B.JPG", 2)),
            permission = {
                checks++
                if (checks <= 2) null else CameraRoll.SELECTED_ONLY_BLOCK
            },
            uploader = { _, _, _, _ ->
                calls++
                UploadVerdict.Verified("nope")
            },
        )
        assertTrue(calls <= 1)
        assertEquals(BackupDisposition.FAILURE, seen.disposition)
        assertTrue(seen.texts.last().contains("Allow all"))
    }

    @Test
    fun stoppedWorkResumesLater() {
        var calls = 0
        val seen = run(
            items = listOf(item("IMG.JPG")),
            stopped = true,
            uploader = { _, _, _, _ ->
                calls++
                UploadVerdict.Verified("nope")
            },
        )
        assertEquals(0, calls)
        assertEquals(BackupDisposition.RETRY, seen.disposition)
        assertTrue(seen.texts.last().contains("resume"))
    }

    @Test
    fun budgetPausesAfterTheFirstVerifiedOriginal() {
        var now = 0L
        var calls = 0
        val seen = run(
            items = listOf(item("A.JPG", id = 1), item("B.JPG", id = 2)),
            budgetMillis = 1_000,
            now = { now },
            uploader = { _, name, sha, _ ->
                calls++
                now = 10_000
                assertEquals("A.JPG", name)
                UploadVerdict.Verified(sha)
            },
        )
        assertEquals(1, calls)
        assertEquals(BackupDisposition.RETRY, seen.disposition)
        assertTrue(seen.texts.last().contains("pausing"))
        assertTrue(seen.progress.isVerified(progressKey(item("A.JPG", id = 1))))
        assertFalse(seen.progress.isVerified(progressKey(item("B.JPG", id = 2))))
    }

    @Test
    fun emptyRollIsStated() {
        val seen = run(items = emptyList(), uploader = { _, _, _, _ -> error("no item") })
        assertEquals(BackupDisposition.SUCCESS, seen.disposition)
        assertTrue(seen.texts.last().contains("no images or videos"))
        assertTrue(seen.texts.last().contains("Failures 0"))
    }

    @Test
    fun readFailureIsVisible() {
        val seen = run(
            items = listOf(item("IMG.JPG")),
            spool = { throw java.io.IOException("original unavailable") },
            uploader = { _, _, _, _ -> UploadVerdict.Verified("nope") },
        )
        assertEquals(BackupDisposition.RETRY, seen.disposition)
        assertTrue(seen.texts.last().contains("original unavailable"))
        assertTrue(seen.texts.last().contains("not changed"))
    }

    @Test
    fun missingPasswordDoesNotMentionABlankStatus() {
        val seen = run(
            items = listOf(item("IMG.JPG")),
            password = "",
            uploader = { _, _, _, _ -> UploadVerdict.Verified("nope") },
        )
        assertEquals(BackupDisposition.FAILURE, seen.disposition)
        assertTrue(seen.texts.last().contains("password"))
        assertTrue(seen.texts.last().isNotBlank())
    }

    @Test
    fun locationDenialOnAGrantedMediaRollBlocksTheRead() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(app).grantPermissions(
            Manifest.permission.READ_MEDIA_IMAGES,
            Manifest.permission.READ_MEDIA_VIDEO,
        )
        assertEquals(CameraRoll.LOCATION_BLOCK, CameraRoll.permissionBlock(app, 33))
    }

    @Test
    fun selectedPhotosOnApi34BlocksTheReadAndSaysAllowAll() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(app).grantPermissions(
            CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED,
            Manifest.permission.ACCESS_MEDIA_LOCATION,
        )
        assertEquals(CameraRoll.SELECTED_ONLY_BLOCK, CameraRoll.permissionBlock(app, 34))
        assertEquals(CameraRoll.MEDIA_BLOCK, CameraRoll.permissionBlock(app, 33))
    }

    @Test
    fun fullAccessOnApi34HasNoBlock() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(app).grantPermissions(
            CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED,
            Manifest.permission.READ_MEDIA_IMAGES,
            Manifest.permission.READ_MEDIA_VIDEO,
            Manifest.permission.ACCESS_MEDIA_LOCATION,
        )
        assertNull(CameraRoll.permissionBlock(app, 34))
    }

    @Test
    fun requestedPermissionsAddSelectedPhotosOnlyFromApi34() {
        assertTrue(CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED in CameraRoll.requestedPermissions(34))
        assertTrue(CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED in CameraRoll.requestedPermissions(35))
        assertFalse(CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED in CameraRoll.requestedPermissions(33))
        assertFalse(CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED in CameraRoll.requiredPermissions(34))
    }

    @Test
    fun mediaDenialSaysTheRollWasNotRead() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(app).grantPermissions(Manifest.permission.ACCESS_MEDIA_LOCATION)
        assertEquals(CameraRoll.MEDIA_BLOCK, CameraRoll.permissionBlock(app, 33))
    }

    private fun item(name: String, id: Long = 1): RollItem = RollItem(
        Uri.parse("content://media/external/file/$id"),
        name,
        3,
        1_700_000_000L,
    )

    private data class Seen(
        val texts: List<String>,
        val disposition: BackupDisposition,
        val progress: MemoryProgressStore,
    )

    private fun run(
        items: List<RollItem>,
        uploader: (File, String, String, Long) -> UploadVerdict,
        password: String = "secret",
        permission: () -> String? = { null },
        stopped: Boolean = false,
        budgetMillis: Long = 60_000,
        now: () -> Long = { 0L },
        progress: MemoryProgressStore = MemoryProgressStore(),
        bytes: ByteArray = "abc".toByteArray(),
        spool: (RollItem) -> Spool = { item ->
            val file = folder.newFile(item.displayName)
            file.writeBytes(bytes)
            Spool(file, Sha256.hex(bytes), item.displayName)
        },
    ): Seen {
        val texts = mutableListOf<String>()
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val disposition = executeBackup(
            context = context,
            endpoint = "http://127.0.0.1:9/",
            user = "camera",
            passwordReader = { password },
            pageSize = 32,
            budgetMillis = budgetMillis,
            mismatchAttempts = 3,
            isStopped = { stopped },
            nowMillis = now,
            pageAt = { offset, _ -> if (offset == 0) items else emptyList() },
            spoolItem = spool,
            uploaderFactory = { _, _, _ ->
                OriginalUploader { file, filename, sha, mtime -> uploader(file, filename, sha, mtime) }
            },
            progress = progress,
            permissionBlock = permission,
            publish = { report -> texts += report.text() },
        )
        return Seen(texts, disposition, progress)
    }
}
