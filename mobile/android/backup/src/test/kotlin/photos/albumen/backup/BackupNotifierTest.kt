// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.Manifest
import android.app.Application
import android.app.Notification
import android.app.NotificationManager
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class BackupNotifierTest {
    @Test
    fun aMismatchIsStoredAndNotified() {
        val context = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val report = BackupReport()
        report.fail(
            "SHA-256 mismatch for IMG.JPG. Client aa. Server bb. The camera roll was not changed. Backup will retry.",
            retryLater = true,
        )
        publishReport(context, report)
        val stored = BackupStatusStore.read(context)
        assertTrue(stored.contains("Client aa"))
        assertTrue(stored.contains("Server bb"))
        assertTrue(stored.contains("Failures 1"))
        val manager = context.getSystemService(NotificationManager::class.java)
        val posted = manager.activeNotifications.toList()
        assertTrue(posted.isNotEmpty())
        val shown = posted.joinToString("\n") { note ->
            val extras = note.notification.extras
            listOf(
                extras.getCharSequence(Notification.EXTRA_BIG_TEXT),
                extras.getCharSequence(Notification.EXTRA_TEXT),
            ).joinToString(" ")
        }
        assertTrue(shown.contains("aa"))
        assertTrue(shown.contains("bb"))
    }

    @Test
    fun statusFileReplacesThePreviousText() {
        val dir = java.io.File("build/status-replace").apply { mkdirs() }
        BackupStatusStore.write(dir, "first failure\nVerified 0 this run. Already verified 0. Failures 1.")
        BackupStatusStore.write(dir, "second failure\nVerified 0 this run. Already verified 0. Failures 1.")
        val text = BackupStatusStore.read(dir)
        assertTrue(text.contains("second failure"))
        assertTrue(!text.contains("first failure"))
    }
}
