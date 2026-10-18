// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.os.Build
import android.os.Bundle
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import photos.albumen.backup.CameraRoll
import photos.albumen.backup.CameraRollBackupWorker

/**
 * Requests [CameraRoll.requiredPermissions] and enqueues backup only when the
 * gate allows it. The permission contract receives permission names. The
 * receiver password stays in a field until [CameraRollBackupWorker.enqueue]
 * stores it in the Keystore. CI does not click the system dialog; the gate is
 * unit-tested.
 */
class HostActivity : ComponentActivity() {
    private lateinit var status: TextView
    private var endpoint: String = ""
    private var user: String = ""
    private var password: String = ""

    private val requestPermissions =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            val decision = HostSession.onPermissionResult(
                sdkInt = Build.VERSION.SDK_INT,
                grants = grants,
                endpoint = endpoint,
                user = user,
                password = password,
            ) { endpoint, user, password ->
                CameraRollBackupWorker.enqueue(this, endpoint, user, password)
            }
            status.text = decision.message
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        status = TextView(this)
        setContentView(status)
        endpoint = intent.getStringExtra(EXTRA_ENDPOINT).orEmpty()
        user = intent.getStringExtra(EXTRA_USER).orEmpty()
        password = intent.getStringExtra(EXTRA_PASSWORD).orEmpty()
        requestPermissions.launch(CameraRoll.requiredPermissions())
    }

    companion object {
        const val EXTRA_ENDPOINT = "photos.albumen.host.ENDPOINT"
        const val EXTRA_USER = "photos.albumen.host.USER"
        const val EXTRA_PASSWORD = "photos.albumen.host.PASSWORD"
    }
}
