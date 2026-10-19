// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.content.Context
import android.content.pm.ServiceInfo
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.WorkManager
import androidx.work.WorkerParameters

/**
 * Background camera-roll backup. A verified upload deletes the scratch file in
 * the app cache. It does not delete, update, or rewrite the MediaStore row.
 * Every pass writes a status the host screen can show, and posts a notification.
 */
class CameraRollBackupWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun getForegroundInfo(): ForegroundInfo {
        val notification = BackupNotifier.ongoing(applicationContext)
        return ForegroundInfo(
            BackupNotifier.NOTIFICATION_ID,
            notification,
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
    }

    override suspend fun doWork(): Result {
        val report = BackupReport()
        val publish = { current: BackupReport -> publishReport(applicationContext, current) }
        try {
            setForeground(getForegroundInfo())
        } catch (thrown: Throwable) {
            report.note(
                "The ongoing notification could not be posted: ${failureText(thrown)}. Failures stay on the status screen.",
            )
            publish(report)
        }
        return try {
            val disposition = executeBackup(
                context = applicationContext,
                endpoint = inputData.getString(KEY_ENDPOINT).orEmpty(),
                user = inputData.getString(KEY_USER).orEmpty(),
                pageSize = inputData.getLong(KEY_PAGE_SIZE, BackupSchedule.DEFAULT_PAGE_SIZE),
                budgetMillis = inputData.getLong(KEY_BUDGET_MILLIS, BackupSchedule.DEFAULT_BUDGET_MILLIS),
                isStopped = { isStopped },
                publish = publish,
                report = report,
            )
            val output = statusOutput(report.summaryLine())
            val periodic = inputData.getBoolean(KEY_PERIODIC, false)
            when (disposition) {
                BackupDisposition.SUCCESS -> Result.success(output)
                // Idle periodic work cannot carry backoff. The next window resumes.
                BackupDisposition.RETRY -> if (periodic) Result.success(output) else Result.retry()
                BackupDisposition.FAILURE -> Result.failure(output)
            }
        } catch (thrown: Throwable) {
            report.fail(
                "Backup stopped: ${failureText(thrown)}. The camera roll was not changed.",
                retryLater = true,
            )
            try {
                publish(report)
            } catch (publishError: Throwable) {
                if (BackupStatusStore.read(applicationContext).isBlank()) {
                    throw publishError
                }
            }
            Result.retry()
        }
    }

    companion object {
        const val UNIQUE_NAME = "albumen-camera-roll"
        const val KEY_ENDPOINT = "endpoint"
        const val KEY_USER = "user"
        const val KEY_PAGE_SIZE = "pageSize"
        const val KEY_BUDGET_MILLIS = "budgetMillis"
        const val KEY_PERIODIC = "periodic"
        const val KEY_STATUS = "status"

        /**
         * Queue the first backup immediately and keep a periodic backup for
         * later. The password is written to [ReceiverPasswordStore] and is not
         * part of the WorkManager input. [pageSize] and [budgetMillis] are soft
         * defaults; a larger value is a longer page or a longer pass, not a cap.
         */
        fun enqueue(
            context: Context,
            endpoint: String,
            user: String,
            password: String,
            pageSize: Long = BackupSchedule.DEFAULT_PAGE_SIZE,
            budgetMillis: Long = BackupSchedule.DEFAULT_BUDGET_MILLIS,
        ) {
            ReceiverPasswordStore.save(context, password)
            val manager = WorkManager.getInstance(context)
            manager.enqueueUniqueWork(
                BackupSchedule.FIRST_NAME,
                ExistingWorkPolicy.REPLACE,
                BackupSchedule.firstRequest(endpoint, user, password, pageSize, budgetMillis),
            )
            manager.enqueueUniquePeriodicWork(
                UNIQUE_NAME,
                ExistingPeriodicWorkPolicy.UPDATE,
                BackupSchedule.periodicRequest(endpoint, user, password, pageSize, budgetMillis),
            )
        }
    }
}

fun publishReport(context: Context, report: BackupReport) {
    val text = report.text()
    check(text.isNotBlank()) { "backup status is empty" }
    try {
        BackupStatusStore.write(context, text)
    } catch (writeError: Throwable) {
        try {
            BackupNotifier.show(context, text, alert = true)
        } catch (notifyError: RuntimeException) {
            if (BackupStatusStore.read(context).isBlank()) {
                throw writeError
            }
        }
        return
    }
    try {
        BackupNotifier.show(context, text, alert = report.failures.isNotEmpty())
    } catch (notifyError: RuntimeException) {
        if (BackupStatusStore.read(context).isBlank()) {
            throw notifyError
        }
    }
}
