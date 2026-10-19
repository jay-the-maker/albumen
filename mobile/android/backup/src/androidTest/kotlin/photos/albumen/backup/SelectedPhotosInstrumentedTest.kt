// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.Manifest
import android.content.ContentResolver
import android.content.ContentValues
import android.content.pm.PackageManager
import android.os.Build
import android.provider.MediaStore
import androidx.core.content.ContextCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/**
 * Android 14 Select photos, on a real API 34 emulator. The OS reports only
 * `READ_MEDIA_VISUAL_USER_SELECTED`, so a pass reads nothing and says Allow
 * all. After full access is granted, the same pass copies a synthetic JPEG
 * to a loopback receiver with byte-identical GPS EXIF.
 *
 * `scripts/android-emulator-selected-photos.sh` runs this class by itself on a
 * fresh install, before the other connected tests grant full access. In the
 * full connected run it is skipped once full access is already granted.
 * Granting a permission does not restart the process; revoking one would, so
 * this test only grants.
 */
@RunWith(AndroidJUnit4::class)
class SelectedPhotosInstrumentedTest {
    @Test(timeout = 120_000)
    fun selectedPhotosWaitsForAllowAllAndThenBacksUpTheOriginal() {
        assumeTrue("Select photos exists from API 34", Build.VERSION.SDK_INT >= 34)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val ui = instrumentation.uiAutomation
        val pkg = context.packageName
        assumeTrue(
            "this class needs a fresh install without full media access",
            !granted(context, Manifest.permission.READ_MEDIA_IMAGES),
        )

        ui.grantRuntimePermission(pkg, CameraRoll.READ_MEDIA_VISUAL_USER_SELECTED)
        ui.grantRuntimePermission(pkg, Manifest.permission.ACCESS_MEDIA_LOCATION)
        assertEquals(CameraRoll.SELECTED_ONLY_BLOCK, CameraRoll.permissionBlock(context))

        val resolver = context.contentResolver
        val cache = context.cacheDir
        LoopbackWebDav { _, body ->
            LoopbackWebDav.Reply(201, Sha256.hex(body), "Created")
        }.use { server ->
            val reports = mutableListOf<String>()
            val disposition = pass(context, server, resolver, cache, emptyList(), reports)
            assertEquals(reports.joinToString("\n"), BackupDisposition.FAILURE, disposition)
            assertTrue(reports.last().contains("Allow all"))
            assertEquals(0, server.bodies.size)
        }

        // Allow all: the person grants full photo and video access.
        ui.grantRuntimePermission(pkg, Manifest.permission.READ_MEDIA_IMAGES)
        ui.grantRuntimePermission(pkg, Manifest.permission.READ_MEDIA_VIDEO)
        assertNull(CameraRoll.permissionBlock(context))

        val jpeg = SyntheticGpsJpeg.bytes()
        val name = "AlbumenSelected${System.currentTimeMillis()}.jpg"
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, name)
            put(MediaStore.Images.Media.MIME_TYPE, "image/jpeg")
            put(MediaStore.Images.Media.RELATIVE_PATH, "DCIM/AlbumenSynthetic")
            put(MediaStore.Images.Media.DATE_MODIFIED, 1_700_000_000L)
        }
        val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
        assertNotNull(uri)
        resolver.openOutputStream(uri!!).use { stream -> checkNotNull(stream).write(jpeg) }
        val found = find(resolver, name)
        val sourceExif = SyntheticGpsJpeg.exifApp1(jpeg)

        LoopbackWebDav { _, body ->
            LoopbackWebDav.Reply(201, Sha256.hex(body), "Created")
        }.use { server ->
            val reports = mutableListOf<String>()
            val disposition = pass(context, server, resolver, cache, listOf(found), reports)
            assertEquals(reports.joinToString("\n"), BackupDisposition.SUCCESS, disposition)
            assertTrue(reports.last().contains("Verified 1 this run."))
            assertTrue(reports.last().contains("Failures 0."))
            val received = server.bodies.single()
            assertEquals(Sha256.hex(jpeg), Sha256.hex(received))
            assertArrayEquals(
                "GPS EXIF of the received file differs from the source",
                sourceExif,
                SyntheticGpsJpeg.exifApp1(received),
            )
        }
        assertNotNull(find(resolver, name))
    }

    private fun pass(
        context: android.content.Context,
        server: LoopbackWebDav,
        resolver: ContentResolver,
        cache: File,
        items: List<RollItem>,
        reports: MutableList<String>,
    ): BackupDisposition = executeBackup(
        context = context,
        endpoint = server.endpoint(),
        user = "camera",
        passwordReader = { "secret" },
        pageSize = 32,
        budgetMillis = 60_000,
        isStopped = { false },
        pageAt = { offset, _ -> if (offset == 0) items else emptyList() },
        spoolItem = { item -> CameraRoll.spool(resolver, item, cache) },
        uploaderFactory = { endpoint, user, password -> WebDavUploader(endpoint, user, password) },
        progress = FileProgressStore(File(cache, "progress-${UUID.randomUUID()}.txt")),
        permissionBlock = { CameraRoll.permissionBlock(context) },
        publish = { report -> reports += report.text() },
    )

    private fun granted(context: android.content.Context, permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

    private fun find(resolver: ContentResolver, name: String): RollItem {
        var offset = 0
        while (offset < 10_000) {
            val page = CameraRoll.page(resolver, offset, 50)
            if (page.isEmpty()) {
                break
            }
            page.firstOrNull { it.displayName == name }?.let { return it }
            offset += page.size
        }
        throw AssertionError("synthetic image was not listed from MediaStore")
    }
}
