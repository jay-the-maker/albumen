// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import androidx.core.app.NotificationCompat

/**
 * One backup status notification, replaced in place. A failure alerts. A later
 * update of a success does not add another notification. There is no rating
 * prompt and no second channel. While a pass runs, WorkManager shows
 * [ongoing] under [ONGOING_NOTIFICATION_ID]. WorkManager removes that ID with
 * `stopForeground(true)` when the pass ends, so the status keeps its own ID.
 */
object BackupNotifier {
    const val CHANNEL_ID = "albumen-backup"
    const val NOTIFICATION_ID = 1001

    /** Foreground-service notification for a running pass. Never the status. */
    const val ONGOING_NOTIFICATION_ID = 1002

    fun show(context: Context, text: String, alert: Boolean) {
        require(text.isNotBlank()) { "notification text is empty" }
        ensureChannel(context)
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.notify(NOTIFICATION_ID, build(context, text, alert))
    }

    fun ongoing(context: Context): Notification {
        ensureChannel(context)
        return build(
            context,
            "Albumen is copying originals. The camera roll is not changed.",
            alert = false,
        )
    }

    private fun build(context: Context, text: String, alert: Boolean): Notification {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
        val pending = launch?.let { intent ->
            PendingIntent.getActivity(
                context,
                0,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        }
        return NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("Albumen backup")
            .setContentText(text.lineSequence().first())
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setContentIntent(pending)
            .setOnlyAlertOnce(!alert)
            .setAutoCancel(false)
            .build()
    }

    private fun ensureChannel(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL_ID) != null) {
            return
        }
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Backup",
            NotificationManager.IMPORTANCE_DEFAULT,
        )
        channel.description = "Albumen backup status. Failures are posted here."
        manager.createNotificationChannel(channel)
    }
}
