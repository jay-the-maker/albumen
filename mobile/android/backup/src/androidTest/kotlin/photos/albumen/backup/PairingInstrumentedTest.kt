// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** The emulator runs the same pairing parse as the JVM unit test. */
@RunWith(AndroidJUnit4::class)
class PairingInstrumentedTest {
    @Test
    fun wrongFingerprintIsVisibleAndTheCopyDoesNotStart() {
        val code = Pairing.parse(Pairing.EXAMPLE_WITH_MEMBER)
        assertEquals("https://127.0.0.1:8443/", code.endpoint)
        assertEquals("b".repeat(64), code.member)
        val wrong = "f".repeat(64)
        val sentence = Pairing.pin(code.fingerprint, wrong)
        assertTrue(sentence!!.contains("fingerprint"))
        assertTrue(sentence.contains("The copy did not start."))
        assertFalse(sentence.contains("camera-secret"))
        assertNull(Pairing.pin(code.fingerprint, code.fingerprint))
    }
}
