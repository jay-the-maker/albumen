// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import androidx.work.Data
import androidx.work.workDataOf

/**
 * WorkManager input for a camera-roll backup. The receiver password is an
 * argument so a caller cannot forget it, and it is not written into [Data].
 * [pageSize] and [budgetMillis] are stored so a caller can raise the soft defaults.
 */
internal fun backupInput(
    endpoint: String,
    user: String,
    password: String,
    pageSize: Long = BackupSchedule.DEFAULT_PAGE_SIZE,
    budgetMillis: Long = BackupSchedule.DEFAULT_BUDGET_MILLIS,
): Data {
    check(password.isNotEmpty()) { "receiver password is empty" }
    return workDataOf(
        CameraRollBackupWorker.KEY_ENDPOINT to endpoint,
        CameraRollBackupWorker.KEY_USER to user,
        CameraRollBackupWorker.KEY_PAGE_SIZE to pageSize,
        CameraRollBackupWorker.KEY_BUDGET_MILLIS to budgetMillis,
    )
}

internal fun statusOutput(summary: String): Data {
    check(summary.isNotBlank()) { "backup status is empty" }
    return workDataOf(CameraRollBackupWorker.KEY_STATUS to summary)
}
