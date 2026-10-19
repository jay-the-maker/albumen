// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.Manifest
import android.app.Application
import android.app.Notification
import android.app.NotificationManager
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config
import java.io.File

/**
 * A phone backup as member A writes `members/<A>/` through the real WebDAV
 * client. Member B's originals, mtime, and sidecar stay byte-identical.
 * A destination outside A's prefix is refused before the request and named
 * in the status file and notification 1001.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class BackupFamilyNamespaceTest {
    @get:Rule
    val folder = TemporaryFolder()

    @Test(timeout = 60_000)
    fun memberABackupLeavesMemberBByteIdenticalAndRefusesAnOutsideWrite() {
        val context = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val library = folder.newFolder("family")
        val original = File(library, "members/$MEMBER_B/objects/sha256/ab")
        val sidecar = File(library, "members/$MEMBER_B/objects/sha256/ab.sidecar")
        original.parentFile!!.mkdirs()
        val originalBytes = "member-b-original".toByteArray()
        val sidecarBytes = "<x:xmpmeta member-b/>".toByteArray()
        original.writeBytes(originalBytes)
        sidecar.writeBytes(sidecarBytes)
        original.setLastModified(1_700_000_000_000L)
        sidecar.setLastModified(1_700_000_111_000L)
        val before = snapshot(original, sidecar)

        val phoneBytes = "member-a-phone-backup".toByteArray()
        var requestLine = ""
        LoopbackWebDav { headers, body ->
            requestLine = headers.lineSequence().first()
            val relative = requestLine.substringAfter(' ').substringBefore(' ')
            check(relative.startsWith("/members/$MEMBER_A/")) { requestLine }
            check(!relative.contains(MEMBER_B)) { requestLine }
            val stored = File(library, relative.trimStart('/'))
            stored.parentFile!!.mkdirs()
            stored.writeBytes(body)
            LoopbackWebDav.Reply(201, Sha256.hex(body), "Created")
        }.use { server ->
            val seen = backup(
                context = context,
                endpoint = server.endpoint(),
                memberId = MEMBER_A,
                name = "phone.jpg",
                bytes = phoneBytes,
            )
            assertEquals(BackupDisposition.SUCCESS, seen)
            assertTrue(requestLine.contains("/members/$MEMBER_A/phone.jpg"))
            val stored = File(library, "members/$MEMBER_A/phone.jpg")
            assertTrue(stored.isFile)
            assertEquals(Sha256.hex(phoneBytes), Sha256.hex(stored.readBytes()))
        }
        assertEquals(before, snapshot(original, sidecar))

        var outsideCalls = 0
        LoopbackWebDav { _, _ ->
            outsideCalls++
            LoopbackWebDav.Reply(500, "aa", "should not be called")
        }.use { server ->
            val endpoint = server.endpoint().trimEnd('/') + "/members/$MEMBER_B/"
            backup(
                context = context,
                endpoint = endpoint,
                memberId = MEMBER_A,
                name = "outside.jpg",
                bytes = phoneBytes,
            )
            assertEquals(0, outsideCalls)
            val stored = BackupStatusStore.read(context)
            val shown = notificationText(context)
            assertTrue(stored.contains("outside.jpg"))
            assertTrue(stored.contains("outside members/$MEMBER_A/"))
            assertTrue(shown.contains("outside.jpg"))
            assertTrue(shown.contains("outside members/$MEMBER_A/"))
        }
        assertEquals(before, snapshot(original, sidecar))
    }

    private fun backup(
        context: Application,
        endpoint: String,
        memberId: String,
        name: String,
        bytes: ByteArray,
    ): BackupDisposition {
        val item = RollItem(Uri.parse("content://media/external/file/$name"), name, bytes.size.toLong(), 1_700_000_000L)
        return executeBackup(
            context = context,
            endpoint = endpoint,
            user = "camera",
            passwordReader = { "receiver-password" },
            memberId = memberId,
            pageAt = { offset, _ -> if (offset == 0) listOf(item) else emptyList() },
            spoolItem = {
                val file = folder.newFile("spool-$name-${System.nanoTime()}")
                file.writeBytes(bytes)
                Spool(file, Sha256.hex(bytes), name)
            },
            progress = MemoryProgressStore(),
            permissionBlock = { null },
            publish = { report -> publishReport(context, report) },
        )
    }

    private fun snapshot(original: File, sidecar: File): List<String> = listOf(original, sidecar).map { file ->
        "${file.name} ${file.lastModified()} ${Sha256.hex(file.readBytes())}"
    }

    private fun notificationText(context: Application): String {
        val manager = context.getSystemService(NotificationManager::class.java)
        val note = manager.activeNotifications.firstOrNull { it.id == BackupNotifier.NOTIFICATION_ID }
            ?: return ""
        val extras = note.notification.extras
        return listOf(
            extras.getCharSequence(Notification.EXTRA_BIG_TEXT),
            extras.getCharSequence(Notification.EXTRA_TEXT),
        ).filterNotNull().joinToString("\n")
    }

    private companion object {
        const val MEMBER_A = "1111111111111111111111111111111111111111111111111111111111111111"
        const val MEMBER_B = "2222222222222222222222222222222222222222222222222222222222222222"
    }
}
