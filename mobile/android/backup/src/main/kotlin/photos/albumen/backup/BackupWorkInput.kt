// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import androidx.work.Data
import androidx.work.workDataOf

/**
 * WorkManager input for a camera-roll backup. The receiver password is an
 * argument so a caller cannot forget it, and it is not written into [Data].
 */
internal fun backupInput(endpoint: String, user: String, password: String): Data {
    check(password.isNotEmpty()) { "receiver password is empty" }
    return workDataOf(
        CameraRollBackupWorker.KEY_ENDPOINT to endpoint,
        CameraRollBackupWorker.KEY_USER to user,
    )
}
