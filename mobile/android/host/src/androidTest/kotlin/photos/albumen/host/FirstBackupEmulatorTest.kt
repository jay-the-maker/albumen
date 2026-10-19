// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.FileObserver
import android.os.SystemClock
import android.provider.MediaStore
import android.view.ViewGroup
import android.widget.TextView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.lifecycle.Observer
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
import java.util.Base64
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
 * screen and in notification [BackupNotifier.NOTIFICATION_ID]. Those checks
 * block until the status file and the notification record the failure, then
 * until a resume shows that failure on the screen. The first resume can still
 * be the permission decision. The wait is bounded by the same deadline.
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

    @Test(timeout = 180_000)
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

    @Test(timeout = 180_000)
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

    @Test(timeout = 120_000)
    fun pairingQrReachesAVerifiedCopyWithinFiveSteps() {
        val context = targetContext()
        val steps = phoneFrictionSteps(readFrictionSteps())
        assertTrue("phone friction is ${steps.size} steps", steps.size <= 5)
        assertFalse(steps.any { it.contains("account") || it.contains("login") || it.contains("signup") })
        val name = "albumen-synthetic-pair.jpg"
        val jpeg = insertSynthetic(context, name)
        val sourceExif = SyntheticGpsJpeg.exifApp1(jpeg)
        val fingerprint = "ab".repeat(32)
        val token = "cd".repeat(32)
        LoopbackWebDav { _, body ->
            LoopbackWebDav.Reply(201, Sha256.hex(body), "Created")
        }.use { server ->
            val https = server.endpoint().replaceFirst("http://", "https://")
            val uri = "albumen-pair://v1?endpoint=${percentEncode(https)}&fingerprint=$fingerprint&token=$token"
            val performed = mutableListOf<String>()
            fun step(id: String) {
                val expected = steps.getOrNull(performed.size)
                assertEquals("step ${performed.size + 1}", expected, id)
                performed += id
            }
            val startedAt = SystemClock.elapsedRealtime()
            assertNotNull(context.packageManager.getPackageInfo(context.packageName, 0))
            step("install")
            launchWithoutReceiver().use { scenario ->
                step("open")
                val granted = awaitScreen(scenario) { text ->
                    text.isNotBlank() && !text.startsWith("Requesting access")
                }
                assertFalse(granted.contains("account", ignoreCase = true))
                step("grant-media-and-location")
                scenario.onActivity { activity ->
                    activity.pairedUploadEndpoint = { server.endpoint() }
                    assertTrue(activity.onScannedPairing(uri, fingerprint))
                }
                step("scan-pairing-qr")
                assertEquals(steps, performed)
                val info = awaitState(context, WorkInfo.State.SUCCEEDED)
                val elapsed = SystemClock.elapsedRealtime() - startedAt
                assertTrue(
                    "first verified copy took ${elapsed}ms after install; the target is ${BackupSchedule.FIRST_BACKUP_TARGET_MINUTES} minutes",
                    elapsed < TimeUnit.MINUTES.toMillis(BackupSchedule.FIRST_BACKUP_TARGET_MINUTES.toLong()),
                )
                assertEquals(WorkInfo.State.SUCCEEDED, info.state)
                val received = checkNotNull(server.bodies.firstOrNull { it.contentEquals(jpeg) }) {
                    "receiver did not store the synthetic original"
                }
                assertArrayEquals(jpeg, received)
                assertArrayEquals(sourceExif, SyntheticGpsJpeg.exifApp1(received))
                val basic = Base64.getEncoder().encodeToString("camera:$token".toByteArray(Charsets.UTF_8))
                assertTrue(server.headerBlocks.any { it.contains(basic) })
                val screen = screenText(scenario)
                assertTrue(screen.contains("Verified "))
                assertFalse(screen.contains("Verified 0 this run."))
                assertTrue(screen.contains("Failures 0."))
                assertFalse(screen.contains("account", ignoreCase = true))
                assertCameraRollUnchanged(context, name, jpeg, sourceExif)
            }
        }
    }

    private fun launchWithoutReceiver(): ActivityScenario<HostActivity> =
        ActivityScenario.launch(Intent(targetContext(), HostActivity::class.java))

    private fun awaitScreen(
        scenario: ActivityScenario<HostActivity>,
        ready: (String) -> Boolean,
    ): String {
        val deadline = SystemClock.elapsedRealtime() + 20_000
        var text = ""
        while (SystemClock.elapsedRealtime() < deadline) {
            text = screenText(scenario)
            if (ready(text)) {
                return text
            }
            SystemClock.sleep(50)
        }
        throw AssertionError("screen stayed: $text")
    }

    private fun readFrictionSteps(): String =
        InstrumentationRegistry.getInstrumentation().context.assets
            .open("phone-friction-steps.txt")
            .bufferedReader()
            .use { it.readText() }

    private fun phoneFrictionSteps(text: String): List<String> =
        text.lineSequence().map { it.trim() }.filter { line -> line.isNotEmpty() && !line.startsWith("#") }.toList()

    private fun percentEncode(text: String): String = buildString {
        for (byte in text.toByteArray(Charsets.UTF_8)) {
            val value = byte.toInt() and 0xFF
            val ch = value.toChar()
            if (ch.isLetterOrDigit() || ch == '-' || ch == '.' || ch == '_' || ch == '~') {
                append(ch)
            } else {
                append('%')
                append("0123456789ABCDEF"[value ushr 4])
                append("0123456789ABCDEF"[value and 0x0F])
            }
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

    /**
     * Wait until [BackupStatusStore] and notification [BackupNotifier.NOTIFICATION_ID]
     * both carry [marker], then until the status screen does too. The file wait
     * wakes when the status file is renamed and when WorkManager reports a new
     * state. A closed port can sit in the connect timeout for each attempt, so
     * the bound covers that run. The activity draws the stored status on resume.
     * The resume that follows the permission grant can still show only
     * [PermissionGate.GRANTED], so the screen is read again until the failure is
     * on it or the deadline passes. [assertFailureSurfaced] still requires the
     * failure line, a failure count above zero, and the same line in the notification.
     */
    private fun awaitFailure(context: Context, scenario: ActivityScenario<HostActivity>, marker: String): Surfaced {
        val deadline = SystemClock.elapsedRealtime() + FAILURE_WAIT_MS
        awaitPublished(context, marker, deadline)
        var screen = ""
        var notification = ""
        while (SystemClock.elapsedRealtime() < deadline) {
            screen = screenText(scenario)
            notification = notificationText(context)
            if (marker in screen && marker in notification && failureCount(screen) > 0 && failureCount(notification) > 0) {
                return Surfaced(screen, notification)
            }
            val slice = (deadline - SystemClock.elapsedRealtime()).coerceAtMost(SIGNAL_SLICE_MS)
            if (slice <= 0) {
                break
            }
            SystemClock.sleep(slice)
        }
        throw AssertionError(
            "failure was not surfaced. screen: $screen\nnotification: $notification\n" +
                "store: ${BackupStatusStore.read(context)}\nwork: ${workState(context)}",
        )
    }

    private fun awaitPublished(context: Context, marker: String, deadline: Long) {
        val lock = Object()
        val watcher = object : FileObserver(context.filesDir, FileObserver.MOVED_TO or FileObserver.CLOSE_WRITE) {
            override fun onEvent(event: Int, path: String?) {
                synchronized(lock) { lock.notifyAll() }
            }
        }
        val live = WorkManager.getInstance(context).getWorkInfosForUniqueWorkLiveData(BackupSchedule.FIRST_NAME)
        val observer = Observer<List<WorkInfo>> {
            synchronized(lock) { lock.notifyAll() }
        }
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync { live.observeForever(observer) }
        watcher.startWatching()
        try {
            while (SystemClock.elapsedRealtime() < deadline) {
                val stored = BackupStatusStore.read(context)
                val notification = notificationText(context)
                val state = workState(context)
                if (marker in stored && marker in notification && failureCount(stored) > 0 && failureCount(notification) > 0) {
                    return
                }
                if ((state == WorkInfo.State.FAILED.name || state == WorkInfo.State.CANCELLED.name) && marker !in stored) {
                    throw AssertionError(
                        "first backup $state before [$marker] was stored. store: $stored notification: $notification",
                    )
                }
                val slice = (deadline - SystemClock.elapsedRealtime()).coerceAtMost(SIGNAL_SLICE_MS)
                if (slice <= 0) {
                    break
                }
                try {
                    synchronized(lock) { lock.wait(slice) }
                } catch (interrupted: InterruptedException) {
                    Thread.currentThread().interrupt()
                    throw AssertionError("interrupted while waiting for [$marker]")
                }
            }
        } finally {
            watcher.stopWatching()
            instrumentation.runOnMainSync { live.removeObserver(observer) }
        }
        throw AssertionError(
            "failure was not published before the bound. store: ${BackupStatusStore.read(context)} " +
                "notification: ${notificationText(context)} work: ${workState(context)}",
        )
    }

    private fun workState(context: Context): String =
        WorkManager.getInstance(context)
            .getWorkInfosForUniqueWork(BackupSchedule.FIRST_NAME)
            .get(10, TimeUnit.SECONDS)
            .firstOrNull()
            ?.state
            ?.name
            ?: "missing"

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

    private companion object {
        /** Three connect attempts at the receiver's connect timeout, plus the pass around them. */
        const val FAILURE_WAIT_MS = 150_000L

        /** Upper bound on each wait so a missed file event is checked again. The wait returns early on a signal. */
        const val SIGNAL_SLICE_MS = 500L
    }
}
