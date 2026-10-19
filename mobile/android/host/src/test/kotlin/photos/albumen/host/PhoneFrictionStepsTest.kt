// SPDX-License-Identifier: Apache-2.0

package photos.albumen.host

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** The Android and iOS CI jobs count the same step list. More than five fails. */
class PhoneFrictionStepsTest {
    @Test
    fun androidAndIosListsMatchAndStayWithinFive() {
        val android = File("src/androidTest/assets/phone-friction-steps.txt").readText()
        val ios = File("../../ios/SimulatorTests/phone-friction-steps.txt").readText()
        assertEquals(ios, android)
        val steps = android.lineSequence().map { it.trim() }.filter { line ->
            line.isNotEmpty() && !line.startsWith("#")
        }.toList()
        assertEquals(
            listOf("install", "open", "grant-media-and-location", "scan-pairing-qr"),
            steps,
        )
        assertTrue("phone friction is ${steps.size} steps", steps.size <= 5)
        assertFalse(steps.any { it.contains("account") || it.contains("login") || it.contains("signup") })
    }
}
