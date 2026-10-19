// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.Manifest
import android.app.Application
import android.app.Notification
import android.app.NotificationManager
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Repeated backup cycles against a loopback receiver. Faults are a closed
 * port, a socket closed during the PUT body, a kill that stops the pass
 * during that upload, and a 201 whose SHA-256 is not the bytes that were
 * sent. Recovery cycles then run with a healthy receiver.
 *
 * A silent failure is an item this cycle tried to upload that is still
 * unverified when the cycle ends, and whose filename is absent from that
 * cycle's status file and from notification 1001. The file is replaced on
 * each publish, so the test keeps each cycle's text. After the fault cycles,
 * every still-unverified filename has to be in that record. After recovery,
 * every item is verified.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class BackupSoakTest {
    @get:Rule
    val folder = TemporaryFolder()

    @Test(timeout = 180_000)
    fun repeatedCyclesRecoverAndSurfaceEveryMiss() {
        val context = ApplicationProvider.getApplicationContext<Application>()
        Shadows.shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val items = library(ITEM_COUNT)
        val sources = items.associate { item ->
            val bytes = ByteArray(ITEM_BYTES) { index -> (item.displayName.hashCode() + index).toByte() }
            val file = folder.newFile("source-${item.displayName}")
            file.writeBytes(bytes)
            item.displayName to bytes
        }
        val progressFile = folder.newFile("progress.txt")
        progressFile.delete()
        var progress = FileProgressStore(progressFile)
        val snapshots = mutableListOf<String>()
        var silentFailures = 0
        var cycles = 0
        var faultWindowVerified = 0

        repeat(SEQUENCES) { sequence ->
            if (sequence > 0) {
                progressFile.delete()
                progress = FileProgressStore(progressFile)
            }
            val stopped = AtomicBoolean(false)
            val faults = listOf(
                Cycle("receiver-down", SoakReceiver.Fault.DROP, down = true),
                Cycle("network-drop", SoakReceiver.Fault.DROP, down = false),
                Cycle("kill-mid-upload", SoakReceiver.Fault.KILL, down = false),
                Cycle("sha256-mismatch", SoakReceiver.Fault.MISMATCH, down = false),
            )
            for (cycle in faults) {
                val outcome = runCycle(
                    context = context,
                    items = items,
                    sources = sources,
                    progress = progress,
                    stopped = stopped,
                    cycle = cycle,
                )
                cycles++
                snapshots += outcome.surfaced
                silentFailures += outcome.silent
                assertEquals("cycle ${cycle.name} silent failures", 0, outcome.silent)
                assertCycleShape(cycle, outcome)
            }
            faultWindowVerified = verifiedCount(items, progress)
            assertEquals(0, faultWindowVerified)
            val faultRecord = snapshots.takeLast(faults.size).joinToString("\n")
            for (item in items) {
                assertTrue(
                    "${item.displayName} was unverified after the fault cycles and absent from the status file and notification",
                    faultRecord.contains(item.displayName),
                )
            }
            val recovered = runCycle(
                context = context,
                items = items,
                sources = sources,
                progress = progress,
                stopped = stopped,
                cycle = Cycle("healthy-recovery", SoakReceiver.Fault.HEALTHY, down = false),
            )
            cycles++
            snapshots += recovered.surfaced
            silentFailures += recovered.silent
            assertEquals(0, recovered.silent)
            assertEquals(ITEM_COUNT, recovered.attempted.size)
            assertEquals(ITEM_COUNT, verifiedCount(items, progress))
            assertTrue(recovered.surfaced.contains("Failures 0"))
        }

        for (item in items) {
            val bytes = sources.getValue(item.displayName)
            val source = File(folder.root, "source-${item.displayName}")
            assertTrue(source.readBytes().contentEquals(bytes))
            assertTrue(progress.isVerified(progressKey(item)))
        }
        val unverified = items.filter { !progress.isVerified(progressKey(it)) }
        val record = snapshots.joinToString("\n")
        for (item in unverified) {
            assertTrue(record.contains(item.displayName))
        }
        assertEquals(0, silentFailures)
        val verified = verifiedCount(items, progress)
        val line = resultLine(verified, items.size, cycles, silentFailures, faultWindowVerified)
        val out = File("build/backup-soak-result.txt")
        out.parentFile?.mkdirs()
        out.writeText(line + "\n")
        println(line)
        assertEquals(ITEM_COUNT, verified)
        assertTrue(line.contains("silent_failures=0"))
    }

    private fun assertCycleShape(cycle: Cycle, outcome: CycleOutcome) {
        when (cycle.name) {
            "receiver-down" -> {
                assertEquals(ITEM_COUNT, outcome.attempted.size)
                assertEquals(0, outcome.verifiedDelta)
                assertTrue(outcome.surfaced.contains("Upload of soak-item-01.jpg failed"))
            }
            "network-drop" -> {
                assertEquals(ITEM_COUNT, outcome.attempted.size)
                assertEquals(0, outcome.verifiedDelta)
                assertTrue(outcome.shortCloses >= ITEM_COUNT)
                assertTrue(outcome.surfaced.contains("Upload of soak-item-01.jpg failed"))
            }
            "kill-mid-upload" -> {
                assertEquals(1, outcome.attempted.size)
                assertEquals(0, outcome.verifiedDelta)
                assertTrue(outcome.shortCloses >= 1)
                val name = outcome.attempted.single()
                assertTrue(outcome.surfaced.contains("Upload of $name failed"))
                assertTrue(outcome.surfaced.contains("stopped before every original was verified"))
                assertTrue(outcome.attempted.size < ITEM_COUNT)
            }
            "sha256-mismatch" -> {
                assertEquals(ITEM_COUNT, outcome.attempted.size)
                assertEquals(0, outcome.verifiedDelta)
                assertTrue(outcome.surfaced.contains("SHA-256 mismatch for soak-item-01.jpg"))
                assertTrue(outcome.surfaced.contains("Server ${SoakReceiver.MISMATCH_SHA}"))
            }
            else -> error(cycle.name)
        }
    }

    private fun runCycle(
        context: Application,
        items: List<RollItem>,
        sources: Map<String, ByteArray>,
        progress: FileProgressStore,
        stopped: AtomicBoolean,
        cycle: Cycle,
    ): CycleOutcome {
        stopped.set(false)
        val verifiedBefore = verifiedCount(items, progress)
        val attempted = linkedSetOf<String>()
        val endpoint: String
        val receiver: SoakReceiver?
        if (cycle.down) {
            endpoint = "http://127.0.0.1:${SoakReceiver.closedPort()}/"
            receiver = null
        } else {
            val opened = SoakReceiver(
                fault = { cycle.fault },
                onKill = { stopped.set(true) },
            )
            endpoint = opened.endpoint()
            receiver = opened
        }
        try {
            executeBackup(
                context = context,
                endpoint = endpoint,
                user = "camera",
                passwordReader = { "receiver-password" },
                pageSize = 32,
                budgetMillis = 600_000,
                mismatchAttempts = 3,
                isStopped = { stopped.get() },
                pageAt = { offset, _ -> if (offset == 0) items else emptyList() },
                spoolItem = { item ->
                    val bytes = sources.getValue(item.displayName)
                    val file = folder.newFile("spool-${cycle.name}-${item.displayName}-${System.nanoTime()}")
                    file.writeBytes(bytes)
                    Spool(file, Sha256.hex(bytes), item.displayName)
                },
                uploaderFactory = { url, user, password ->
                    val real = WebDavUploader(url, user, password)
                    OriginalUploader { file, filename, sha, mtime ->
                        attempted += filename
                        real.put(file, filename, sha, mtime)
                    }
                },
                progress = progress,
                permissionBlock = { null },
                publish = { report -> publishReport(context, report) },
            )
        } finally {
            receiver?.close()
        }
        val stored = BackupStatusStore.read(context)
        val shown = notificationText(context)
        val surfaced = "$stored\n$shown"
        assertTrue("status file was blank after ${cycle.name}", stored.isNotBlank())
        assertTrue("notification 1001 was blank after ${cycle.name}", shown.isNotBlank())
        val verifiedAfter = verifiedCount(items, progress)
        var silent = 0
        for (name in attempted) {
            val item = items.single { it.displayName == name }
            if (!progress.isVerified(progressKey(item)) && !surfaced.contains(name)) {
                silent++
            }
        }
        return CycleOutcome(
            attempted = attempted,
            surfaced = surfaced,
            silent = silent,
            verifiedDelta = verifiedAfter - verifiedBefore,
            shortCloses = receiver?.shortCloseCount() ?: 0,
        )
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

    private fun library(count: Int): List<RollItem> = (1..count).map { id ->
        RollItem(
            Uri.parse("content://media/external/file/$id"),
            "soak-item-%02d.jpg".format(id),
            ITEM_BYTES.toLong(),
            1_700_000_000L + id,
        )
    }

    private fun verifiedCount(items: List<RollItem>, progress: ProgressStore): Int =
        items.count { progress.isVerified(progressKey(it)) }

    private fun resultLine(
        verified: Int,
        items: Int,
        cycles: Int,
        silentFailures: Int,
        faultWindowVerified: Int,
    ): String {
        val scaled = verified * 1000L / items
        val faultScaled = faultWindowVerified * 1000L / items
        return "backup-soak verified_percent=${scaled / 10}.${scaled % 10} " +
            "verified=$verified items=$items cycles=$cycles silent_failures=$silentFailures " +
            "fault_window_verified_percent=${faultScaled / 10}.${faultScaled % 10}"
    }

    private data class Cycle(val name: String, val fault: SoakReceiver.Fault, val down: Boolean)

    private data class CycleOutcome(
        val attempted: Set<String>,
        val surfaced: String,
        val silent: Int,
        val verifiedDelta: Int,
        val shortCloses: Int,
    )

    private companion object {
        const val ITEM_COUNT = 24
        const val ITEM_BYTES = 256 * 1024
        const val SEQUENCES = 2
    }
}
