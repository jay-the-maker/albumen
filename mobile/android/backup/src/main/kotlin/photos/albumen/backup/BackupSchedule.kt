// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.PeriodicWorkRequest
import androidx.work.PeriodicWorkRequestBuilder
import java.util.concurrent.TimeUnit

/**
 * The first job is how a verified copy can finish within
 * [FIRST_BACKUP_TARGET_MINUTES] of setup: no initial delay, network only, and
 * expedited when the system still has quota. The periodic job is the battery
 * default and waits until the phone is idle, charging, and on a network.
 * WorkManager enforces both constraint sets. Neither request carries the
 * receiver password.
 */
object BackupSchedule {
    const val FIRST_BACKUP_TARGET_MINUTES = 5
    const val PERIODIC_MINUTES = 15L
    const val DEFAULT_PAGE_SIZE = 32L
    const val DEFAULT_BUDGET_MILLIS = 4L * 60L * 1000L
    const val DEFAULT_MISMATCH_ATTEMPTS = 3
    const val FIRST_NAME = "albumen-camera-roll-first"
    const val PROGRESS_FILE = "albumen-backup-progress.txt"
    const val FIRST_RUN_MESSAGE =
        "The first backup is queued. It runs on a network and is aimed at a verified copy within 5 minutes of setup. Later backups run while the phone is idle, charging, and on a network."

    fun firstConstraints(): Constraints = Constraints.Builder()
        .setRequiredNetworkType(NetworkType.CONNECTED)
        .build()

    fun periodicConstraints(): Constraints = Constraints.Builder()
        .setRequiresCharging(true)
        .setRequiresDeviceIdle(true)
        .setRequiredNetworkType(NetworkType.CONNECTED)
        .build()

    fun firstRequest(
        endpoint: String,
        user: String,
        password: String,
        pageSize: Long = DEFAULT_PAGE_SIZE,
        budgetMillis: Long = DEFAULT_BUDGET_MILLIS,
    ): OneTimeWorkRequest = OneTimeWorkRequestBuilder<CameraRollBackupWorker>()
        .setConstraints(firstConstraints())
        .setInputData(backupInput(endpoint, user, password, pageSize, budgetMillis))
        .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
        .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
        .build()

    fun periodicRequest(
        endpoint: String,
        user: String,
        password: String,
        pageSize: Long = DEFAULT_PAGE_SIZE,
        budgetMillis: Long = DEFAULT_BUDGET_MILLIS,
    ): PeriodicWorkRequest = PeriodicWorkRequestBuilder<CameraRollBackupWorker>(
        PERIODIC_MINUTES,
        TimeUnit.MINUTES,
    )
        .setConstraints(periodicConstraints())
        .setInputData(backupInput(endpoint, user, password, pageSize, budgetMillis, periodic = true))
        .build()
}
