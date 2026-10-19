// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import android.Manifest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

class PermissionGateTest {
    @Test
    fun deniedImagesOrVideoDoesNotEnqueue() {
        val grants = mapOf(
            Manifest.permission.READ_MEDIA_IMAGES to false,
            Manifest.permission.READ_MEDIA_VIDEO to true,
            Manifest.permission.ACCESS_MEDIA_LOCATION to true,
        )
        val calls = mutableListOf<Triple<String, String, String>>()
        val decision = HostSession.onPermissionResult(34, grants, "https://albumen.example", "camera", "secret") { endpoint, user, password ->
            calls.add(Triple(endpoint, user, password))
        }
        assertFalse(decision.enqueue)
        assertFalse(decision.backupFinished)
        assertEquals(PermissionGate.MEDIA_DENIED, decision.message)
        assertTrue(calls.isEmpty())
    }

    @Test
    fun legacyStorageDenialDoesNotEnqueue() {
        val grants = mapOf(
            Manifest.permission.READ_EXTERNAL_STORAGE to false,
            Manifest.permission.ACCESS_MEDIA_LOCATION to true,
        )
        val decision = PermissionGate.decide(30, grants)
        assertFalse(decision.enqueue)
        assertFalse(decision.backupFinished)
    }

    @Test
    fun locationDenialExplainsStrippedGpsAndDoesNotFinish() {
        val grants = mapOf(
            Manifest.permission.READ_MEDIA_IMAGES to true,
            Manifest.permission.READ_MEDIA_VIDEO to true,
            Manifest.permission.ACCESS_MEDIA_LOCATION to false,
        )
        val calls = mutableListOf<Triple<String, String, String>>()
        val decision = HostSession.onPermissionResult(33, grants, "https://albumen.example", "camera", "secret") { endpoint, user, password ->
            calls.add(Triple(endpoint, user, password))
        }
        assertFalse(decision.enqueue)
        assertFalse(decision.backupFinished)
        assertTrue(decision.message.contains("GPS"))
        assertEquals(PermissionGate.LOCATION_DENIED, decision.message)
        assertTrue(calls.isEmpty())
    }

    @Test
    fun selectedPhotosGrantIsNotEnough() {
        val grants = mapOf(
            "android.permission.READ_MEDIA_VISUAL_USER_SELECTED" to true,
            Manifest.permission.ACCESS_MEDIA_LOCATION to true,
        )
        val decision = PermissionGate.decide(34, grants)
        assertFalse(decision.enqueue)
        assertFalse(decision.backupFinished)
    }

    @Test
    fun fullGrantEnqueuesWithoutPuttingThePasswordInTheGrantMap() {
        val secret = "receiver-password-not-for-the-permission-request"
        val grants = mapOf(
            Manifest.permission.READ_EXTERNAL_STORAGE to true,
            Manifest.permission.ACCESS_MEDIA_LOCATION to true,
        )
        assertFalse(grants.keys.any { it.contains(secret) })
        assertFalse(grants.values.any { it.toString().contains(secret) })
        val calls = mutableListOf<Triple<String, String, String>>()
        val decision = HostSession.onPermissionResult(31, grants, "https://albumen.example", "camera", secret) { endpoint, user, password ->
            calls.add(Triple(endpoint, user, password))
        }
        assertTrue(decision.enqueue)
        assertTrue(decision.backupFinished)
        assertEquals(listOf(Triple("https://albumen.example", "camera", secret)), calls)
    }

    @Test
    fun missingReceiverDoesNotEnqueue() {
        val grants = mapOf(
            Manifest.permission.READ_MEDIA_IMAGES to true,
            Manifest.permission.READ_MEDIA_VIDEO to true,
            Manifest.permission.ACCESS_MEDIA_LOCATION to true,
        )
        val calls = mutableListOf<Triple<String, String, String>>()
        val decision = HostSession.onPermissionResult(33, grants, "", "camera", "secret") { endpoint, user, password ->
            calls.add(Triple(endpoint, user, password))
        }
        assertFalse(decision.enqueue)
        assertFalse(decision.backupFinished)
        assertEquals(HostSession.MISSING_RECEIVER, decision.message)
        assertTrue(calls.isEmpty())
    }

    @Test
    fun emptyPasswordDoesNotEnqueue() {
        val grants = mapOf(
            Manifest.permission.READ_MEDIA_IMAGES to true,
            Manifest.permission.READ_MEDIA_VIDEO to true,
            Manifest.permission.ACCESS_MEDIA_LOCATION to true,
        )
        val calls = mutableListOf<Triple<String, String, String>>()
        val decision = HostSession.onPermissionResult(33, grants, "https://albumen.example", "camera", "") { endpoint, user, password ->
            calls.add(Triple(endpoint, user, password))
        }
        assertFalse(decision.enqueue)
        assertFalse(decision.backupFinished)
        assertEquals(HostSession.MISSING_PASSWORD, decision.message)
        assertTrue(calls.isEmpty())
    }

    @Test
    fun activityLaunchesOnlyTheRequiredPermissions() {
        val activity = File("src/main/kotlin/photos/albumen/host/HostActivity.kt").readText()
        assertTrue(activity.contains("requestPermissions.launch(CameraRoll.requiredPermissions())"))
        assertTrue(activity.contains("HostSession.onPermissionResult"))
        assertTrue(activity.contains("BackupStatusStore.read"))
        assertTrue(activity.contains("BackupSchedule.FIRST_RUN_MESSAGE"))
        assertFalse(activity.contains("workDataOf"))
        assertFalse(activity.contains("RequestMultiplePermissions(password"))
        val gate = File("src/main/kotlin/photos/albumen/host/PermissionGate.kt").readText()
        assertFalse(gate.contains("workDataOf"))
        assertFalse(gate.contains("KEY_PASSWORD"))
    }
}
