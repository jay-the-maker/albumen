// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/**
 * Background camera-roll backup. A verified upload deletes the scratch file in
 * the app cache. It does not delete, update, or rewrite the MediaStore row.
 */
class CameraRollBackupWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val endpoint = inputData.getString(KEY_ENDPOINT) ?: return Result.failure()
        val user = inputData.getString(KEY_USER) ?: return Result.failure()
        val password = ReceiverPasswordStore.read(applicationContext) ?: return Result.failure()
        val uploader = WebDavUploader(endpoint, user, password)
        val items = try {
            CameraRoll.list(applicationContext.contentResolver)
        } catch (_: Exception) {
            return Result.retry()
        }
        var failed = false
        for (item in items) {
            val spool = try {
                CameraRoll.spool(
                    applicationContext.contentResolver,
                    item,
                    applicationContext.cacheDir,
                )
            } catch (_: Exception) {
                failed = true
                continue
            }
            try {
                when (uploader.put(spool.file, spool.filename, spool.sha256)) {
                    is UploadVerdict.Verified -> Unit
                    else -> failed = true
                }
            } catch (_: Exception) {
                failed = true
            } finally {
                spool.file.delete()
            }
        }
        return if (failed) Result.retry() else Result.success()
    }

    companion object {
        const val UNIQUE_NAME = "albumen-camera-roll"
        const val KEY_ENDPOINT = "endpoint"
        const val KEY_USER = "user"

        /**
         * Run while the device is idle and charging. The password is written to
         * [ReceiverPasswordStore] and is not part of the WorkManager input.
         */
        fun enqueue(context: Context, endpoint: String, user: String, password: String) {
            ReceiverPasswordStore.save(context, password)
            val constraints = Constraints.Builder()
                .setRequiresCharging(true)
                .setRequiresDeviceIdle(true)
                .setRequiredNetworkType(NetworkType.CONNECTED)
                .build()
            val request = PeriodicWorkRequestBuilder<CameraRollBackupWorker>(15, TimeUnit.MINUTES)
                .setConstraints(constraints)
                .setInputData(backupInput(endpoint, user, password))
                .build()
            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                UNIQUE_NAME,
                ExistingPeriodicWorkPolicy.UPDATE,
                request,
            )
        }
    }
}
