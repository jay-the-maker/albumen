// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import androidx.work.NetworkType
import androidx.work.OutOfQuotaPolicy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class BackupScheduleTest {
    @Test
    fun firstJobCanStartWithinFiveMinutesOfSetup() {
        assertEquals(5, BackupSchedule.FIRST_BACKUP_TARGET_MINUTES)
        assertTrue(BackupSchedule.FIRST_RUN_MESSAGE.contains("5 minutes"))
        val secret = "receiver-password-not-for-workmanager"
        val request = BackupSchedule.firstRequest(
            "https://albumen.example",
            "camera",
            secret,
            pageSize = 100_000,
            budgetMillis = 60_000,
        )
        val spec = request.workSpec
        assertEquals(NetworkType.CONNECTED, spec.constraints.requiredNetworkType)
        assertFalse(spec.constraints.requiresCharging())
        assertFalse(spec.constraints.requiresDeviceIdle())
        assertTrue(spec.expedited)
        assertEquals(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST, spec.outOfQuotaPolicy)
        assertEquals(0L, spec.initialDelay)
        assertEquals(100_000L, spec.input.getLong(CameraRollBackupWorker.KEY_PAGE_SIZE, 0))
        assertFalse(spec.input.keyValueMap.values.any { it.toString().contains(secret) })
    }

    @Test
    fun periodicJobStaysIdleChargingAndOnANetwork() {
        val request = BackupSchedule.periodicRequest("https://albumen.example", "camera", "secret")
        val spec = request.workSpec
        assertTrue(spec.constraints.requiresCharging())
        assertTrue(spec.constraints.requiresDeviceIdle())
        assertEquals(NetworkType.CONNECTED, spec.constraints.requiredNetworkType)
        assertEquals(TimeUnit.MINUTES.toMillis(15), spec.intervalDuration)
        assertFalse(spec.expedited)
        assertTrue(spec.input.getBoolean(CameraRollBackupWorker.KEY_PERIODIC, false))
    }

    @Test
    fun pageSizeIsASoftDefault() {
        assertEquals(32, pageLimit(0))
        assertEquals(100_000, pageLimit(100_000))
        assertEquals(Int.MAX_VALUE, pageLimit(Long.MAX_VALUE))
    }
}
