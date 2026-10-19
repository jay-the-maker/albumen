// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.Manifest
import photos.albumen.backup.CameraRoll

/**
 * What the host does after the system permission dialog returns.
 *
 * Images or video denied: do not enqueue. Media granted and location denied:
 * MediaStore would strip GPS, so the backup is not started and is not finished.
 * Select photos on Android 14 and newer: the backup waits for full access and
 * the host offers Allow all. The receiver password is not an input to this
 * decision.
 */
data class PermissionDecision(
    val enqueue: Boolean,
    val backupFinished: Boolean,
    val message: String,
    /** Show the one Allow all action that asks again for full access. */
    val allowAll: Boolean = false,
)

object PermissionGate {
    const val MEDIA_DENIED =
        "Images and video access was denied. Backup was not started."
    const val LOCATION_DENIED =
        "Location access was denied, so MediaStore would strip GPS. That copy is not a complete original. Backup was not started and is not finished."
    const val SELECTED_ONLY = CameraRoll.SELECTED_ONLY_BLOCK
    const val GRANTED =
        "Media and location access were granted. Backup can run."

    fun decide(sdkInt: Int, grants: Map<String, Boolean>): PermissionDecision {
        val required = CameraRoll.requiredPermissions(sdkInt).toList()
        val media = required.filter { it != Manifest.permission.ACCESS_MEDIA_LOCATION }
        val mediaGranted = media.isNotEmpty() && media.all { grants[it] == true }
        if (!mediaGranted) {
            if (CameraRoll.isSelectedOnly(sdkInt) { grants[it] == true }) {
                return PermissionDecision(
                    enqueue = false,
                    backupFinished = false,
                    message = SELECTED_ONLY,
                    allowAll = true,
                )
            }
            return PermissionDecision(enqueue = false, backupFinished = false, message = MEDIA_DENIED)
        }
        if (grants[Manifest.permission.ACCESS_MEDIA_LOCATION] != true) {
            return PermissionDecision(
                enqueue = false,
                backupFinished = false,
                message = LOCATION_DENIED,
            )
        }
        return PermissionDecision(enqueue = true, backupFinished = true, message = GRANTED)
    }
}

/**
 * Applies a permission result. Enqueue runs only when [PermissionGate] says
 * the backup may start. The password is handed to that callback and is not
 * written into the grant map.
 */
object HostSession {
    const val MISSING_RECEIVER =
        "Receiver endpoint or user is missing. Backup was not started."
    const val MISSING_PASSWORD =
        "Receiver password is missing. It is not part of the permission request."

    fun onPermissionResult(
        sdkInt: Int,
        grants: Map<String, Boolean>,
        endpoint: String,
        user: String,
        password: String,
        enqueue: (String, String, String) -> Unit,
    ): PermissionDecision {
        val decision = PermissionGate.decide(sdkInt, grants)
        if (!decision.enqueue) {
            return decision
        }
        if (endpoint.isBlank() || user.isBlank()) {
            return PermissionDecision(
                enqueue = false,
                backupFinished = false,
                message = MISSING_RECEIVER,
            )
        }
        if (password.isEmpty()) {
            return PermissionDecision(
                enqueue = false,
                backupFinished = false,
                message = MISSING_PASSWORD,
            )
        }
        enqueue(endpoint, user, password)
        return decision
    }
}
