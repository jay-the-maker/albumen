// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.Manifest
import android.os.Build
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import photos.albumen.backup.BackupSchedule
import photos.albumen.backup.BackupStatusStore
import photos.albumen.backup.CameraRoll
import photos.albumen.backup.CameraRollBackupWorker
import photos.albumen.backup.Pairing
import photos.albumen.backup.PairingCode
import photos.albumen.backup.PairingException

/**
 * Requests [CameraRoll.requestedPermissions] and enqueues backup only when the
 * gate allows it. When Android 14 or newer reports Select photos, the screen
 * says so and shows one Allow all button that asks for full access again. The permission contract receives permission names. The
 * receiver password stays in a field until [CameraRollBackupWorker.enqueue]
 * stores it in the Keystore. The screen shows that decision and every line
 * the worker has written. CI does not click the system dialog; the gate is
 * unit-tested.
 */
class HostActivity : ComponentActivity() {
    private lateinit var status: TextView
    private lateinit var allowAll: Button
    private var allowAllShown: Boolean = false
    private var endpoint: String = ""
    private var user: String = ""
    private var password: String = ""
    private var memberId: String = ""

    /**
     * Where a scanned pairing code is uploaded. Production uses the endpoint
     * in the QR. A test may point that at a loopback on the same host and port.
     */
    internal var pairedUploadEndpoint: (PairingCode) -> String = { it.endpoint }

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
            allowAllShown = decision.allowAll
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

    /**
     * Entry a camera QR scanner calls with the decoded `albumen-pair://` text
     * and the certificate fingerprint the handshake presented. The one-time
     * token is the receiver password. The receiver user is the name printed
     * beside the QR. There is no account field.
     */
    fun onScannedPairing(uri: String, presentedFingerprint: String): Boolean {
        val code = try {
            Pairing.parse(uri)
        } catch (error: PairingException) {
            decisionMessage = error.message ?: "The pairing code is not an albumen-pair v1 URI. The copy did not start."
            render()
            return false
        }
        val mismatch = Pairing.pin(code.fingerprint, presentedFingerprint)
        if (mismatch != null) {
            decisionMessage = mismatch
            render()
            return false
        }
        val blocked = CameraRoll.permissionBlock(this)
        if (blocked != null) {
            decisionMessage = blocked
            allowAllShown = blocked == CameraRoll.SELECTED_ONLY_BLOCK
            render()
            return false
        }
        CameraRollBackupWorker.enqueue(
            this,
            pairedUploadEndpoint(code),
            RECEIVER_USER,
            code.token,
            memberId = code.member.orEmpty(),
        )
        decisionMessage = "Pairing code accepted. Backup can run.\n\n" + BackupSchedule.FIRST_RUN_MESSAGE
        render()
        return true
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val scroll = ScrollView(this)
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
        }
        status = TextView(this).apply {
            setPadding(48, 48, 48, 48)
        }
        allowAll = Button(this).apply {
            text = ALLOW_ALL
            visibility = View.GONE
            setOnClickListener { requestAccess() }
        }
        column.addView(status)
        column.addView(allowAll)
        scroll.addView(column)
        setContentView(scroll)
        endpoint = intent.getStringExtra(EXTRA_ENDPOINT).orEmpty()
        user = intent.getStringExtra(EXTRA_USER).orEmpty()
        password = intent.getStringExtra(EXTRA_PASSWORD).orEmpty()
        memberId = intent.getStringExtra(EXTRA_MEMBER).orEmpty()
        render()
        requestAccess()
    }

    /**
     * Ask for access. With full access already granted this is the required
     * set, which returns at once without a dialog.
     */
    internal fun requestAccess() {
        val permissions = if (CameraRoll.permissionBlock(this) == null) {
            CameraRoll.requiredPermissions()
        } else {
            CameraRoll.requestedPermissions()
        }
        requestPermissions.launch(permissions)
    }

    override fun onResume() {
        super.onResume()
        if (::status.isInitialized) {
            if (allowAllShown && CameraRoll.permissionBlock(this) == null) {
                // Full access was granted in Settings. Run the same gate again.
                requestAccess()
            }
            render()
        }
    }

    private fun render() {
        allowAll.visibility = if (allowAllShown) View.VISIBLE else View.GONE
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
        const val ALLOW_ALL = "Allow all"
        const val NOTIFICATION_DENIED =
            "Notification permission was denied. Backup failures stay on this screen."

        /** The receiver user printed beside the pairing QR. It is not an account. */
        const val RECEIVER_USER = "camera"
    }
}
