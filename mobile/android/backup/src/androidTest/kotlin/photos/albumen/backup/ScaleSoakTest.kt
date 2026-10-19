// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.app.Notification
import android.app.NotificationManager
import android.net.Uri
import android.os.SystemClock
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/**
 * Seeded soak of synthetic items on a booted emulator. The camera roll is not
 * opened. Bytes live in the test cache. The receiver stores only the item
 * name that was uploaded. A `members/` path is refused.
 *
 * Power loss closes the socket and stops the pass. The next pass reads the
 * progress file and continues. That is the stand-in for a killed worker.
 * Rebooting the emulator on every fault would not finish the shard.
 *
 * Without `albumenSoakItems` the test returns. The pull-request emulator job
 * does not pass that argument.
 */
@RunWith(AndroidJUnit4::class)
class ScaleSoakTest {
    @Test(timeout = 50 * 60 * 1000L)
    fun seededFaultsLeaveNoSilentFailure() {
        val items = argument("albumenSoakItems")?.toIntOrNull() ?: 0
        if (items <= 0) {
            return
        }
        val shard = argument("albumenSoakShard")?.toIntOrNull() ?: 0
        val shards = argument("albumenSoakShards")?.toIntOrNull() ?: 1
        val seed = argument("albumenSoakSeed")?.toLongOrNull() ?: 1L
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val cache = context.cacheDir
        val sources = linkedMapOf<String, ByteArray>()
        val library = (0 until items).map { local ->
            val global = shard * items + local
            val name = "scale-s%02d-i%06d.bin".format(shard, local)
            val bytes = ByteArray(ITEM_BYTES) { index -> (global + index).toByte() }
            val source = File(cache, "source-$name")
            source.writeBytes(bytes)
            sources[name] = bytes
            RollItem(
                Uri.parse("content://albumen.scale/item/$global"),
                name,
                bytes.size.toLong(),
                1_700_000_000L + global,
            )
        }
        val byName = library.withIndex().associate { it.value.displayName to it.index }
        val progress = FileProgressStore(File(cache, "scale-progress-$shard.txt"))
        val fault = AtomicReference("healthy")
        val stopped = AtomicBoolean(false)
        val receiver = ScaleSoakReceiver(fault)
        receiver.onPowerLoss = { stopped.set(true) }
        val closed = "http://127.0.0.1:${ScaleSoakReceiver.closedPort()}/"
        val started = SystemClock.elapsedRealtime()
        val verified = linkedSetOf<String>()
        val faulted = linkedSetOf<Int>()
        val snapshots = mutableListOf<String>()
        var silent = 0
        var faultPasses = 0
        var timeTo = -1L
        val timeline = StringBuilder("elapsed_ms,verified,items,silent_failures\n")
        val threshold = (items * 999 + 999) / 1000
        fun note() {
            val elapsed = SystemClock.elapsedRealtime() - started
            timeline.append(elapsed).append(',').append(verified.size).append(',').append(items).append(',').append(silent).append('\n')
            if (timeTo < 0 && verified.size >= threshold) {
                timeTo = elapsed
            }
        }
        try {
            while (faulted.size < items) {
                stopped.set(false)
                faultPasses++
                val attempted = linkedSetOf<String>()
                val report = BackupReport()
                executeBackup(
                    context = context,
                    endpoint = receiver.endpoint(),
                    user = "camera",
                    passwordReader = { "receiver-password" },
                    pageSize = 100_000,
                    budgetMillis = 3_600_000,
                    mismatchAttempts = 3,
                    isStopped = { stopped.get() },
                    memberId = "",
                    pageAt = { offset, _ ->
                        if (offset > 0) {
                            emptyList()
                        } else {
                            library.filter { byName.getValue(it.displayName) !in faulted }
                        }
                    },
                    spoolItem = { item ->
                        val bytes = sources.getValue(item.displayName)
                        val spool = File(cache, "spool-${item.displayName}-${System.nanoTime()}")
                        spool.writeBytes(bytes)
                        Spool(spool, Sha256.hex(bytes), item.displayName)
                    },
                    uploaderFactory = { _, user, password ->
                        OriginalUploader { file, filename, sha, mtime ->
                            val local = byName.getValue(filename)
                            val global = shard * items + local
                            val kind = ScaleSoakSchedule.fault(seed, local, global)
                            faulted += local
                            attempted += filename
                            val verdict = when (kind) {
                                "receiver-down" -> WebDavUploader(closed, user, password).put(file, filename, sha, mtime)
                                else -> {
                                    fault.set(kind)
                                    if (kind == "power-loss") {
                                        stopped.set(false)
                                    }
                                    WebDavUploader(receiver.endpoint(), user, password).put(file, filename, sha, mtime)
                                }
                            }
                            if (verdict is UploadVerdict.Verified) {
                                verified += filename
                                note()
                            }
                            verdict
                        }
                    },
                    progress = progress,
                    permissionBlock = { null },
                    publish = { next ->
                        publishReport(context, next)
                        snapshots += next.text()
                    },
                    report = report,
                )
                var shown = ""
                val noticeDeadline = SystemClock.elapsedRealtime() + 2_000
                do {
                    shown = notificationText(context)
                    if (shown.isNotBlank() || report.failures.isEmpty()) {
                        break
                    }
                    SystemClock.sleep(100)
                } while (SystemClock.elapsedRealtime() < noticeDeadline)
                val surfaced = snapshots.joinToString("\n") + "\n" + shown
                assertFalse("status was blank after fault pass $faultPasses", BackupStatusStore.read(context).isBlank())
                if (report.failures.isNotEmpty()) {
                    assertTrue("notification 1001 was blank after fault pass $faultPasses", shown.isNotBlank())
                }
                for (name in attempted) {
                    if (name !in verified && !surfaced.contains(name)) {
                        silent++
                    }
                }
                note()
                check(attempted.isNotEmpty()) { "fault pass $faultPasses made no progress" }
            }
            stopped.set(false)
            fault.set("healthy")
            val recovery = BackupReport()
            executeBackup(
                context = context,
                endpoint = receiver.endpoint(),
                user = "camera",
                passwordReader = { "receiver-password" },
                pageSize = 100_000,
                budgetMillis = 3_600_000,
                mismatchAttempts = 3,
                isStopped = { false },
                memberId = "",
                pageAt = { offset, _ -> if (offset == 0) library else emptyList() },
                spoolItem = { item ->
                    val bytes = sources.getValue(item.displayName)
                    val spool = File(cache, "spool-recovery-${item.displayName}-${System.nanoTime()}")
                    spool.writeBytes(bytes)
                    Spool(spool, Sha256.hex(bytes), item.displayName)
                },
                uploaderFactory = { endpoint, user, password ->
                    val real = WebDavUploader(endpoint, user, password)
                    OriginalUploader { file, filename, sha, mtime ->
                        fault.set("healthy")
                        val verdict = real.put(file, filename, sha, mtime)
                        if (verdict is UploadVerdict.Verified) {
                            verified += filename
                            note()
                        }
                        verdict
                    }
                },
                progress = progress,
                permissionBlock = { null },
                publish = { next ->
                    publishReport(context, next)
                    snapshots += next.text()
                },
                report = recovery,
            )
            val shown = notificationText(context)
            val surfaced = snapshots.joinToString("\n") + "\n" + shown
            for (item in library) {
                if (!progress.isVerified(progressKey(item)) && !surfaced.contains(item.displayName)) {
                    silent++
                }
            }
            note()
            val elapsed = SystemClock.elapsedRealtime() - started
            val line = resultLine(
                platform = "android",
                shard = shard,
                shards = shards,
                seed = seed,
                verified = verified.size,
                items = items,
                elapsed = elapsed,
                timeTo = timeTo,
                silent = silent,
                faultPasses = faultPasses,
            )
            val faults = buildString {
                append("seed=").append(seed).append('\n')
                append("shard=").append(shard).append('\n')
                append("shards=").append(shards).append('\n')
                append("items=").append(items).append('\n')
                library.forEachIndexed { local, item ->
                    val global = shard * items + local
                    append(local).append(' ').append(global).append(' ').append(item.displayName).append(' ')
                    append(ScaleSoakSchedule.fault(seed, local, global)).append('\n')
                }
            }
            writeOutputs(context, line, faults, timeline.toString())
            Log.i(TAG, line)
            for (item in library) {
                val bytes = sources.getValue(item.displayName)
                val source = File(cache, "source-${item.displayName}")
                assertTrue(source.readBytes().contentEquals(bytes))
                val stored = receiver.stored["/${item.displayName}"]
                assertTrue("missing stored bytes for ${item.displayName}", stored != null && stored.contentEquals(bytes))
            }
            for (path in receiver.paths) {
                assertFalse(path, path.contains("..") || path.contains("members/"))
            }
            assertEquals(0, silent)
            assertEquals(items, verified.size)
            assertTrue(timeTo >= 0)
            assertTrue(line.contains("silent_failures=0"))
        } finally {
            receiver.close()
        }
    }

    private fun writeOutputs(context: android.content.Context, line: String, faults: String, timeline: String) {
        val dirs = mutableListOf(File(context.filesDir, "backup-scale-soak"))
        context.getExternalFilesDir(null)?.let { dirs += File(it, "backup-scale-soak") }
        for (dir in dirs) {
            dir.mkdirs()
            File(dir, "result.txt").writeText(line + "\n")
            File(dir, "faults.log").writeText(faults)
            File(dir, "timeline.csv").writeText(timeline)
        }
    }

    private fun notificationText(context: android.content.Context): String {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return ""
        val note = manager.activeNotifications.firstOrNull { it.id == BackupNotifier.NOTIFICATION_ID } ?: return ""
        val extras = note.notification.extras
        return listOf(
            extras.getCharSequence(Notification.EXTRA_BIG_TEXT),
            extras.getCharSequence(Notification.EXTRA_TEXT),
        ).filterNotNull().joinToString("\n")
    }

    private fun argument(name: String): String? =
        InstrumentationRegistry.getArguments().getString(name)?.takeIf { it.isNotEmpty() }

    private fun resultLine(
        platform: String,
        shard: Int,
        shards: Int,
        seed: Long,
        verified: Int,
        items: Int,
        elapsed: Long,
        timeTo: Long,
        silent: Int,
        faultPasses: Int,
    ): String {
        val scaled = verified * 1000L / items
        return "backup-scale-soak platform=$platform shard=$shard shards=$shards seed=$seed " +
            "items=$items verified=$verified verified_percent=${scaled / 10}.${scaled % 10} " +
            "elapsed_ms=$elapsed time_to_99_9_ms=$timeTo silent_failures=$silent fault_passes=$faultPasses"
    }

    private companion object {
        const val ITEM_BYTES = 128
        const val TAG = "albumen-scale-soak"
    }
}
