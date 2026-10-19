// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.Manifest
import android.os.Build
import android.os.Bundle
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import photos.albumen.backup.BackupSchedule
import photos.albumen.backup.BackupStatusStore
import photos.albumen.backup.CameraRoll
import photos.albumen.backup.CameraRollBackupWorker

/**
 * Requests [CameraRoll.requiredPermissions] and enqueues backup only when the
 * gate allows it. The permission contract receives permission names. The
 * receiver password stays in a field until [CameraRollBackupWorker.enqueue]
 * stores it in the Keystore. The screen shows that decision and every line
 * the worker has written. CI does not click the system dialog; the gate is
 * unit-tested.
 */
class HostActivity : ComponentActivity() {
    private lateinit var status: TextView
    private var endpoint: String = ""
    private var user: String = ""
    private var password: String = ""
    private var memberId: String = ""
    private var decisionMessage: String =
        "Requesting access to images, video, and location so originals, including GPS, can be copied."

    private val requestPermissions =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            val decision = HostSession.onPermissionResult(
                sdkInt = Build.VERSION.SDK_INT,
                grants = grants,
                endpoint = endpoint,
                user = user,
                password = password,
            ) { endpoint, user, password ->
                CameraRollBackupWorker.enqueue(this, endpoint, user, password, memberId = memberId)
            }
            decisionMessage = if (decision.enqueue) {
                decision.message + "\n\n" + BackupSchedule.FIRST_RUN_MESSAGE
            } else {
                decision.message
            }
            render()
            if (Build.VERSION.SDK_INT >= 33) {
                requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }

    private val requestNotifications =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (!granted) {
                decisionMessage = decisionMessage + "\n\n" + NOTIFICATION_DENIED
                render()
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val scroll = ScrollView(this)
        status = TextView(this).apply {
            setPadding(48, 48, 48, 48)
        }
        scroll.addView(status)
        setContentView(scroll)
        endpoint = intent.getStringExtra(EXTRA_ENDPOINT).orEmpty()
        user = intent.getStringExtra(EXTRA_USER).orEmpty()
        password = intent.getStringExtra(EXTRA_PASSWORD).orEmpty()
        memberId = intent.getStringExtra(EXTRA_MEMBER).orEmpty()
        render()
        requestPermissions.launch(CameraRoll.requiredPermissions())
    }

    override fun onResume() {
        super.onResume()
        if (::status.isInitialized) {
            render()
        }
    }

    private fun render() {
        val stored = BackupStatusStore.read(this)
        status.text = buildString {
            append(decisionMessage)
            if (stored.isNotBlank()) {
                append("\n\n")
                append(stored)
            }
        }
    }

    companion object {
        const val EXTRA_ENDPOINT = "photos.albumen.host.ENDPOINT"
        const val EXTRA_USER = "photos.albumen.host.USER"
        const val EXTRA_PASSWORD = "photos.albumen.host.PASSWORD"
        const val EXTRA_MEMBER = "photos.albumen.host.MEMBER"
        const val NOTIFICATION_DENIED =
            "Notification permission was denied. Backup failures stay on this screen."
    }
}
