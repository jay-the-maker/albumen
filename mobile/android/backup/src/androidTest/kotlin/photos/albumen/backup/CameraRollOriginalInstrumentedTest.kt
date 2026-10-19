// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.Manifest
import android.content.ContentValues
import android.os.Build
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/**
 * Inserts a synthetic JPEG, reads it back with `setRequireOriginal`, and PUTs
 * those bytes to a loopback receiver. The GPS EXIF segment the receiver
 * stored is byte-identical to the source. The MediaStore row is still there
 * afterwards, with the same bytes.
 */
@RunWith(AndroidJUnit4::class)
class CameraRollOriginalInstrumentedTest {
    @get:Rule
    val permissions: GrantPermissionRule = if (Build.VERSION.SDK_INT >= 33) {
        GrantPermissionRule.grant(
            Manifest.permission.READ_MEDIA_IMAGES,
            Manifest.permission.READ_MEDIA_VIDEO,
            Manifest.permission.ACCESS_MEDIA_LOCATION,
        )
    } else {
        GrantPermissionRule.grant(
            Manifest.permission.READ_EXTERNAL_STORAGE,
            Manifest.permission.ACCESS_MEDIA_LOCATION,
        )
    }

    @Test(timeout = 60_000)
    fun originalBytesWithGpsAreUploadedAndTheCameraRollStays() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val resolver = context.contentResolver
        assertNull(CameraRoll.permissionBlock(context))
        val jpeg = SyntheticGpsJpeg.bytes()
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, SyntheticGpsJpeg.DISPLAY_NAME)
            put(MediaStore.Images.Media.MIME_TYPE, "image/jpeg")
            put(MediaStore.Images.Media.RELATIVE_PATH, "DCIM/AlbumenSynthetic")
            put(MediaStore.Images.Media.DATE_MODIFIED, 1_700_000_000L)
        }
        val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
        assertNotNull(uri)
        resolver.openOutputStream(uri!!).use { stream ->
            checkNotNull(stream).write(jpeg)
        }

        val found = findInserted(resolver)
        assertEquals(SyntheticGpsJpeg.DISPLAY_NAME, found.displayName)
        val cache = context.cacheDir
        val spooled = CameraRoll.spool(resolver, found, cache)
        try {
            assertEquals(Sha256.hex(jpeg), spooled.sha256)
            assertArrayEquals(jpeg, spooled.file.readBytes())
        } finally {
            spooled.file.delete()
        }

        val sha = Sha256.hex(jpeg)
        val sourceExif = SyntheticGpsJpeg.exifApp1(jpeg)
        LoopbackWebDav { headers, body ->
            assertArrayEquals(jpeg, body)
            assertArrayEquals(
                "GPS EXIF of the received file differs from the source",
                sourceExif,
                SyntheticGpsJpeg.exifApp1(body),
            )
            assertTrue(headers.contains("X-Albumen-Mtime: ${found.mtimeSeconds}"))
            LoopbackWebDav.Reply(201, Sha256.hex(body), "Created")
        }.use { server ->
            val reports = mutableListOf<String>()
            val disposition = executeBackup(
                context = context,
                endpoint = server.endpoint(),
                user = "camera",
                passwordReader = { "secret" },
                pageSize = 32,
                budgetMillis = 60_000,
                isStopped = { false },
                pageAt = { offset, _ -> if (offset == 0) listOf(found) else emptyList() },
                spoolItem = { item -> CameraRoll.spool(resolver, item, cache) },
                uploaderFactory = { endpoint, name, password -> WebDavUploader(endpoint, name, password) },
                progress = FileProgressStore(File(cache, "progress-${UUID.randomUUID()}.txt")),
                permissionBlock = { CameraRoll.permissionBlock(context) },
                publish = { report -> reports += report.text() },
            )
            assertEquals(BackupDisposition.SUCCESS, disposition)
            assertTrue(reports.last().contains("Verified 1 this run."))
            assertTrue(reports.last().contains("Failures 0."))
            assertEquals(1, server.bodies.size)
            val received = server.bodies.single()
            assertEquals(sha, Sha256.hex(received))
            assertArrayEquals(
                "GPS EXIF of the received file differs from the source",
                sourceExif,
                SyntheticGpsJpeg.exifApp1(received),
            )
        }

        val again = CameraRoll.spool(resolver, found, cache)
        try {
            val kept = again.file.readBytes()
            assertArrayEquals(jpeg, kept)
            assertArrayEquals(sourceExif, SyntheticGpsJpeg.exifApp1(kept))
        } finally {
            again.file.delete()
        }
        assertNotNull(findInserted(resolver))
    }

    private fun findInserted(resolver: android.content.ContentResolver): RollItem {
        var offset = 0
        while (offset < 10_000) {
            val page = CameraRoll.page(resolver, offset, 50)
            if (page.isEmpty()) {
                break
            }
            page.firstOrNull { it.displayName == SyntheticGpsJpeg.DISPLAY_NAME }?.let { return it }
            offset += page.size
        }
        throw AssertionError("synthetic image was not listed from MediaStore")
    }
}
