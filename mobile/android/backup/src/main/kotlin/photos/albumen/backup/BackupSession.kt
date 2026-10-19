// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.content.Context
import java.io.File

/**
 * Copies camera-roll originals to an Albumen WebDAV receiver. A copy counts
 * only when `X-Albumen-Sha256` matches the bytes that were sent. A mismatch
 * is retried. The MediaStore row is not deleted or updated. The cache spool
 * is removed after the attempt.
 */
fun executeBackup(
    context: Context,
    endpoint: String,
    user: String,
    passwordReader: () -> String? = { ReceiverPasswordStore.read(context) },
    pageSize: Long = BackupSchedule.DEFAULT_PAGE_SIZE,
    budgetMillis: Long = BackupSchedule.DEFAULT_BUDGET_MILLIS,
    mismatchAttempts: Int = BackupSchedule.DEFAULT_MISMATCH_ATTEMPTS,
    isStopped: () -> Boolean = { false },
    nowMillis: () -> Long = { System.currentTimeMillis() },
    pageAt: (offset: Int, limit: Int) -> List<RollItem> = { offset, limit ->
        CameraRoll.page(context.contentResolver, offset, limit)
    },
    spoolItem: (RollItem) -> Spool = { item ->
        CameraRoll.spool(context.contentResolver, item, context.cacheDir)
    },
    uploaderFactory: (String, String, String) -> OriginalUploader = { endpointUrl, name, password ->
        WebDavUploader(endpointUrl, name, password)
    },
    progress: ProgressStore = FileProgressStore(File(context.filesDir, BackupSchedule.PROGRESS_FILE)),
    permissionBlock: () -> String? = { CameraRoll.permissionBlock(context) },
    publish: (BackupReport) -> Unit,
    report: BackupReport = BackupReport(),
): BackupDisposition {
    try {
        drive(
            report = report,
            endpoint = endpoint,
            user = user,
            passwordReader = passwordReader,
            pageSize = pageSize,
            budgetMillis = budgetMillis,
            mismatchAttempts = mismatchAttempts,
            isStopped = isStopped,
            nowMillis = nowMillis,
            pageAt = pageAt,
            spoolItem = spoolItem,
            uploaderFactory = uploaderFactory,
            progress = progress,
            permissionBlock = permissionBlock,
            publish = publish,
        )
    } catch (thrown: Throwable) {
        report.fail(
            "Backup stopped: ${failureText(thrown)}. The camera roll was not changed.",
            retryLater = true,
        )
    }
    publish(report)
    return report.disposition()
}

private fun drive(
    report: BackupReport,
    endpoint: String,
    user: String,
    passwordReader: () -> String?,
    pageSize: Long,
    budgetMillis: Long,
    mismatchAttempts: Int,
    isStopped: () -> Boolean,
    nowMillis: () -> Long,
    pageAt: (offset: Int, limit: Int) -> List<RollItem>,
    spoolItem: (RollItem) -> Spool,
    uploaderFactory: (String, String, String) -> OriginalUploader,
    progress: ProgressStore,
    permissionBlock: () -> String?,
    publish: (BackupReport) -> Unit,
) {
    if (endpoint.isBlank() || user.isBlank()) {
        report.block("Receiver endpoint or user is missing. Backup did not start.")
        return
    }
    val password = passwordReader()?.takeIf { it.isNotEmpty() }
    if (password == null) {
        report.block(
            "Receiver password is missing from Keystore. It is not in the backup job. Backup did not start.",
        )
        return
    }
    permissionBlock()?.let { message ->
        report.block(message)
        return
    }
    val uploader = uploaderFactory(endpoint, user, password)
    val limit = pageLimit(pageSize)
    val deadline = nowMillis() + budgetMillis.coerceAtLeast(1)
    var offset = 0
    var saw = false
    while (true) {
        if (stopped(isStopped, report)) {
            return
        }
        val page = pageAt(offset, limit)
        if (page.isEmpty()) {
            break
        }
        for ((index, item) in page.withIndex()) {
            if (stopped(isStopped, report)) {
                return
            }
            permissionBlock()?.let { message ->
                report.block(message)
                return
            }
            saw = true
            val keepGoing = uploadOne(
                item = item,
                uploader = uploader,
                progress = progress,
                spoolItem = spoolItem,
                mismatchAttempts = mismatchAttempts,
                report = report,
                publish = publish,
            )
            if (!keepGoing) {
                return
            }
            val more = index < page.lastIndex || page.size == limit
            if (more && nowMillis() >= deadline) {
                report.pause(
                    "Backup is pausing so it can resume. Originals already verified stay verified. The camera roll was not changed.",
                )
                return
            }
        }
        if (page.size < limit) {
            break
        }
        if (offset > Int.MAX_VALUE - page.size) {
            report.fail(
                "MediaStore query offset cannot represent the next page. Verified originals stay verified. The camera roll was not changed. Backup will retry.",
                retryLater = true,
            )
            return
        }
        offset += page.size
    }
    if (!saw && report.failures.isEmpty()) {
        report.note("The camera roll has no images or videos.")
    }
}

private fun stopped(isStopped: () -> Boolean, report: BackupReport): Boolean {
    if (!isStopped()) {
        return false
    }
    report.pause(
        "Backup was stopped before every original was verified. It will resume. The camera roll was not changed.",
    )
    return true
}

/**
 * @return false when the rest of this pass should stop. The camera-roll row
 * is left as it was.
 */
private fun uploadOne(
    item: RollItem,
    uploader: OriginalUploader,
    progress: ProgressStore,
    spoolItem: (RollItem) -> Spool,
    mismatchAttempts: Int,
    report: BackupReport,
    publish: (BackupReport) -> Unit,
): Boolean {
    if (item.displayName.isBlank()) {
        report.fail(
            "A camera-roll row had no filename. It was not uploaded. The camera roll was not changed.",
            retryLater = true,
        )
        publish(report)
        return true
    }
    val name = try {
        StoredName.require(item.displayName)
    } catch (thrown: Exception) {
        report.fail(
            "Refusing a filename for ${item.uri}: ${failureText(thrown)}. The camera roll was not changed.",
            retryLater = true,
        )
        publish(report)
        return true
    }
    if (item.mtimeSeconds < 0) {
        report.fail(
            "$name has no MediaStore modified time. It was not uploaded. The camera roll was not changed.",
            retryLater = true,
        )
        publish(report)
        return true
    }
    val key = progressKey(item)
    if (progress.isVerified(key)) {
        report.skipped++
        return true
    }
    val spool = try {
        spoolItem(item)
    } catch (thrown: Exception) {
        report.fail(
            "Could not read $name: ${failureText(thrown)}. The camera roll was not changed. Backup will retry.",
            retryLater = true,
        )
        publish(report)
        return true
    }
    try {
        var attempt = 0
        while (attempt < mismatchAttempts) {
            attempt++
            val verdict = try {
                uploader.put(spool.file, spool.filename, spool.sha256, item.mtimeSeconds)
            } catch (thrown: Exception) {
                if (attempt >= mismatchAttempts) {
                    report.fail(
                        "Upload of $name failed: ${failureText(thrown)}. The camera roll was not changed. Backup will retry.",
                        retryLater = true,
                    )
                    publish(report)
                }
                continue
            }
            when (verdict) {
                is UploadVerdict.Verified -> {
                    progress.markVerified(key)
                    report.verified++
                    publish(report)
                    return true
                }
                is UploadVerdict.Conflict -> {
                    if (attempt >= mismatchAttempts) {
                        report.fail(
                            "SHA-256 mismatch for $name. Client ${verdict.client}. Server ${verdict.server}. The camera roll was not changed. Backup will retry.",
                            retryLater = true,
                        )
                        publish(report)
                    }
                }
                is UploadVerdict.MissingServerHash -> {
                    if (attempt >= mismatchAttempts) {
                        report.fail(
                            "Receiver did not return X-Albumen-Sha256 for $name. The camera roll was not changed. Backup will retry.",
                            retryLater = true,
                        )
                        publish(report)
                    }
                }
                is UploadVerdict.Rejected -> {
                    if (verdict.status == 401 || verdict.status == 403) {
                        report.block(
                            "Receiver refused $name with HTTP ${verdict.status}. The camera roll was not changed.",
                        )
                        publish(report)
                        return false
                    }
                    if (attempt >= mismatchAttempts) {
                        report.fail(
                            "Receiver refused $name with HTTP ${verdict.status}. The camera roll was not changed. Backup will retry.",
                            retryLater = true,
                        )
                        publish(report)
                    }
                }
            }
        }
        return true
    } finally {
        spool.file.delete()
    }
}

/**
 * MediaStore's limit argument is a 32-bit int. A larger requested page still
 * walks the roll; the query asks for as many rows as that argument can name.
 */
internal fun pageLimit(pageSize: Long): Int = when {
    pageSize <= 0L -> BackupSchedule.DEFAULT_PAGE_SIZE.toInt()
    pageSize > Int.MAX_VALUE -> Int.MAX_VALUE
    else -> pageSize.toInt()
}
