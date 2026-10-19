// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.SystemClock
import android.provider.MediaStore
import android.view.ViewGroup
import android.widget.TextView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import photos.albumen.backup.BackupNotifier
import photos.albumen.backup.BackupSchedule
import photos.albumen.backup.BackupStatusStore
import photos.albumen.backup.CameraRoll
import photos.albumen.backup.LoopbackWebDav
import photos.albumen.backup.Sha256
import photos.albumen.backup.SyntheticGpsJpeg
import java.net.ServerSocket
import java.util.UUID
import java.util.concurrent.TimeUnit

/**
 * Emulator coverage for setup. The host activity is the setup path: it
 * enqueues the first backup only after media and location are granted. The
 * test driver marks that job's network constraint met and does not mark an
 * initial delay, so a delayed or idle-only first job does not finish. The
 * clock starts at launch and must reach a verified PUT inside
 * [BackupSchedule.FIRST_BACKUP_TARGET_MINUTES]. A closed port and a wrong
 * `X-Albumen-Sha256` must show the same non-blank failure on the status
 * screen and in notification [BackupNotifier.NOTIFICATION_ID].
 */
@RunWith(AndroidJUnit4::class)
class FirstBackupEmulatorTest {
    @get:Rule
    val permissions: GrantPermissionRule = if (Build.VERSION.SDK_INT >= 33) {
        GrantPermissionRule.grant(
            Manifest.permission.READ_MEDIA_IMAGES,
            Manifest.permission.READ_MEDIA_VIDEO,
            Manifest.permission.ACCESS_MEDIA_LOCATION,
            Manifest.permission.POST_NOTIFICATIONS,
        )
    } else {
        GrantPermissionRule.grant(
            Manifest.permission.READ_EXTERNAL_STORAGE,
            Manifest.permission.ACCESS_MEDIA_LOCATION,
        )
    }

    @Before
    fun setUpWorkManager() {
        // Real threads, so setForeground can post to the main thread while doWork runs.
        // The test scheduler still withholds an initial delay and a periodic window.
        WorkManagerTestInitHelper.initializeTestWorkManager(
            targetContext(),
            WorkManagerTestInitHelper.ExecutorsMode.PRESERVE_EXECUTORS,
        )
    }

    @Test(timeout = 120_000)
    fun firstVerifiedBackupLandsWithinFiveMinutesOfSetup() {
        val context = targetContext()
        val name = "albumen-synthetic-first.jpg"
        val jpeg = insertSynthetic(context, name)
        val sourceExif = SyntheticGpsJpeg.exifApp1(jpeg)
        LoopbackWebDav { _, body ->
            LoopbackWebDav.Reply(201, Sha256.hex(body), "Created")
        }.use { server ->
            val startedAt = SystemClock.elapsedRealtime()
            launch(server.endpoint()).use { scenario ->
                val info = awaitState(context, WorkInfo.State.SUCCEEDED)
                val elapsed = SystemClock.elapsedRealtime() - startedAt
                assertTrue(
                    "first verified backup took ${elapsed}ms after setup; " +
                        "the target is ${BackupSchedule.FIRST_BACKUP_TARGET_MINUTES} minutes",
                    elapsed < TimeUnit.MINUTES.toMillis(BackupSchedule.FIRST_BACKUP_TARGET_MINUTES.toLong()),
                )
                assertEquals(WorkInfo.State.SUCCEEDED, info.state)
                val received = checkNotNull(server.bodies.firstOrNull { it.contentEquals(jpeg) }) {
                    "receiver did not store the synthetic original"
                }
                assertArrayEquals(
                    "GPS EXIF of the received file differs from the source",
                    sourceExif,
                    SyntheticGpsJpeg.exifApp1(received),
                )
                val screen = screenText(scenario)
                assertTrue(screen.contains("Verified "))
                assertFalse(screen.contains("Verified 0 this run."))
                assertTrue(screen.contains("Failures 0."))
                assertCameraRollUnchanged(context, name, jpeg, sourceExif)
            }
        }
    }

    @Test(timeout = 120_000)
    fun hashMismatchIsOnTheStatusScreenAndTheNotification() {
        val context = targetContext()
        val name = "albumen-synthetic-mismatch.jpg"
        val jpeg = insertSynthetic(context, name)
        val client = Sha256.hex(jpeg)
        val serverHash = "0".repeat(64)
        assertFalse(client == serverHash)
        LoopbackWebDav { _, _ ->
            LoopbackWebDav.Reply(201, serverHash, "Created")
        }.use { server ->
            launch(server.endpoint()).use { scenario ->
                satisfyNetwork(context)
                val marker = "SHA-256 mismatch for $name. Client $client. Server $serverHash."
                val surfaced = awaitFailure(context, scenario, marker)
                assertFailureSurfaced(surfaced.screen, surfaced.notification, marker)
                assertCameraRollUnchanged(context, name, jpeg, SyntheticGpsJpeg.exifApp1(jpeg))
            }
        }
    }

    @Test(timeout = 120_000)
    fun receiverDownIsOnTheStatusScreenAndTheNotification() {
        val context = targetContext()
        val name = "albumen-synthetic-down.jpg"
        val jpeg = insertSynthetic(context, name)
        val endpoint = refusedEndpoint()
        launch(endpoint).use { scenario ->
            satisfyNetwork(context)
            val marker = "Upload of $name failed:"
            val surfaced = awaitFailure(context, scenario, marker)
            assertFailureSurfaced(surfaced.screen, surfaced.notification, marker)
            assertTrue(surfaced.screen.contains("The camera roll was not changed."))
            assertTrue(surfaced.notification.contains("The camera roll was not changed."))
            assertCameraRollUnchanged(context, name, jpeg, SyntheticGpsJpeg.exifApp1(jpeg))
        }
    }

    private fun launch(endpoint: String): ActivityScenario<HostActivity> {
        val intent = Intent(targetContext(), HostActivity::class.java).apply {
            putExtra(HostActivity.EXTRA_ENDPOINT, endpoint)
            putExtra(HostActivity.EXTRA_USER, "camera")
            putExtra(HostActivity.EXTRA_PASSWORD, "secret")
        }
        return ActivityScenario.launch(intent)
    }

    private fun satisfyNetwork(context: Context) {
        val driver = checkNotNull(WorkManagerTestInitHelper.getTestDriver(context)) {
            "test WorkManager was not initialized"
        }
        driver.setAllConstraintsMet(awaitEnqueued(context))
    }

    private fun awaitState(context: Context, wanted: WorkInfo.State): WorkInfo {
        satisfyNetwork(context)
        val manager = WorkManager.getInstance(context)
        val deadline = SystemClock.elapsedRealtime() + 60_000
        var last = "missing"
        while (SystemClock.elapsedRealtime() < deadline) {
            val info = manager.getWorkInfosForUniqueWork(BackupSchedule.FIRST_NAME)
                .get(10, TimeUnit.SECONDS)
                .firstOrNull()
            if (info != null) {
                last = info.state.name
                if (info.state == wanted || info.state == WorkInfo.State.FAILED || info.state == WorkInfo.State.CANCELLED) {
                    return info
                }
            }
            SystemClock.sleep(100)
        }
        throw AssertionError("first backup stayed $last")
    }

    private fun awaitEnqueued(context: Context): UUID {
        val manager = WorkManager.getInstance(context)
        val deadline = SystemClock.elapsedRealtime() + 20_000
        while (SystemClock.elapsedRealtime() < deadline) {
            val id = manager.getWorkInfosForUniqueWork(BackupSchedule.FIRST_NAME)
                .get(10, TimeUnit.SECONDS)
                .firstOrNull()
                ?.id
            if (id != null) {
                return id
            }
            SystemClock.sleep(50)
        }
        throw AssertionError(
            "setup did not enqueue ${BackupSchedule.FIRST_NAME}. Screen file: ${BackupStatusStore.read(context)}",
        )
    }

    private data class Surfaced(val screen: String, val notification: String)

    private fun awaitFailure(context: Context, scenario: ActivityScenario<HostActivity>, marker: String): Surfaced {
        val deadline = SystemClock.elapsedRealtime() + 60_000
        var screen = ""
        var notification = ""
        while (SystemClock.elapsedRealtime() < deadline) {
            screen = screenText(scenario)
            notification = notificationText(context)
            if (marker in screen && marker in notification && failureCount(screen) > 0 && failureCount(notification) > 0) {
                return Surfaced(screen, notification)
            }
            SystemClock.sleep(100)
        }
        throw AssertionError("failure was not surfaced. screen: $screen\nnotification: $notification")
    }

    private fun assertFailureSurfaced(screen: String, notification: String, marker: String) {
        assertTrue("status screen was blank", screen.isNotBlank())
        assertTrue("notification was blank", notification.isNotBlank())
        val line = checkNotNull(screen.lineSequence().firstOrNull { marker in it }) {
            "status screen missing [$marker]. screen: $screen"
        }
        assertTrue(
            "notification missing the status line [$line]. notification: $notification",
            notification.contains(line),
        )
        assertTrue("status screen failure count was ${failureCount(screen)}", failureCount(screen) > 0)
        assertTrue("notification failure count was ${failureCount(notification)}", failureCount(notification) > 0)
        assertFalse(screen.contains("Failures 0."))
        assertFalse(notification.contains("Failures 0."))
    }

    private fun failureCount(text: String): Int =
        Regex("""Failures (\d+)\.""").find(text)?.groupValues?.get(1)?.toInt() ?: -1

    private fun screenText(scenario: ActivityScenario<HostActivity>): String {
        scenario.moveToState(Lifecycle.State.STARTED)
        scenario.moveToState(Lifecycle.State.RESUMED)
        var text = ""
        scenario.onActivity { activity ->
            val content = activity.findViewById<ViewGroup>(android.R.id.content)
            val scroll = content.getChildAt(0) as ViewGroup
            val view = scroll.getChildAt(0) as TextView
            text = view.text?.toString().orEmpty()
        }
        return text
    }

    private fun notificationText(context: Context): String {
        val manager = context.getSystemService(NotificationManager::class.java)
        return manager.activeNotifications
            .filter { it.id == BackupNotifier.NOTIFICATION_ID }
            .joinToString("\n") { posted ->
                val extras = posted.notification.extras
                listOf(
                    extras.getCharSequence(Notification.EXTRA_BIG_TEXT),
                    extras.getCharSequence(Notification.EXTRA_TEXT),
                ).filterNotNull().joinToString("\n")
            }
    }

    private fun insertSynthetic(context: Context, name: String): ByteArray {
        val jpeg = SyntheticGpsJpeg.bytes()
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, name)
            put(MediaStore.Images.Media.MIME_TYPE, "image/jpeg")
            put(MediaStore.Images.Media.RELATIVE_PATH, "DCIM/AlbumenSynthetic")
            put(MediaStore.Images.Media.DATE_MODIFIED, 1_700_000_000L)
        }
        val resolver = context.contentResolver
        val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
        assertNotNull(uri)
        resolver.openOutputStream(uri!!).use { stream ->
            checkNotNull(stream).write(jpeg)
        }
        return jpeg
    }

    private fun assertCameraRollUnchanged(context: Context, name: String, jpeg: ByteArray, sourceExif: ByteArray) {
        val item = findInserted(context, name)
        val spooled = CameraRoll.spool(context.contentResolver, item, context.cacheDir)
        try {
            val kept = spooled.file.readBytes()
            assertArrayEquals(jpeg, kept)
            assertArrayEquals(sourceExif, SyntheticGpsJpeg.exifApp1(kept))
        } finally {
            spooled.file.delete()
        }
    }

    private fun findInserted(context: Context, name: String): photos.albumen.backup.RollItem {
        var offset = 0
        while (offset < 10_000) {
            val page = CameraRoll.page(context.contentResolver, offset, 50)
            if (page.isEmpty()) {
                break
            }
            page.firstOrNull { it.displayName == name }?.let { return it }
            offset += page.size
        }
        throw AssertionError("synthetic image $name was not listed from MediaStore")
    }

    private fun refusedEndpoint(): String {
        val port = ServerSocket(0).use { it.localPort }
        return "http://127.0.0.1:$port/"
    }

    private fun targetContext(): Context = InstrumentationRegistry.getInstrumentation().targetContext
}
