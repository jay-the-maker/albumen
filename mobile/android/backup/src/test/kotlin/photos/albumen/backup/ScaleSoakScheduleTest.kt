// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertEquals
import org.junit.Test

class ScaleSoakScheduleTest {
    @Test
    fun seedOneMatchesTheSimulatorVector() {
        val expected = listOf(
            "network-drop",
            "power-loss",
            "receiver-down",
            "sha-mismatch",
            "network-drop",
            "healthy",
            "receiver-down",
            "healthy",
        )
        val got = (0 until expected.size).map { ScaleSoakSchedule.fault(1L, it, it) }
        assertEquals(expected, got)
        assertEquals("network-drop", ScaleSoakSchedule.fault(1L, 5, 1_005))
    }
}
