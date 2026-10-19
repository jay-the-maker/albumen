// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** Same URI vectors as `receive/src/pairing.rs` and the iOS pairing tests. */
class PairingTest {
    @Test
    fun sharedVectorsParse() {
        val withMember = Pairing.parse(Pairing.EXAMPLE_WITH_MEMBER)
        assertEquals("https://127.0.0.1:8443/", withMember.endpoint)
        assertEquals("a".repeat(64), withMember.token)
        assertEquals("b".repeat(64), withMember.member)
        assertEquals(64, withMember.fingerprint.length)

        val plain = Pairing.parse(Pairing.EXAMPLE_WITHOUT_MEMBER)
        assertEquals("https://photos.example:8443/", plain.endpoint)
        assertNull(plain.member)
    }

    @Test
    fun fingerprintMismatchIsVisibleAndAMatchAllowsTheCopy() {
        val code = Pairing.parse(Pairing.EXAMPLE_WITH_MEMBER)
        assertNull(Pairing.pin(code.fingerprint, code.fingerprint.uppercase()))
        val wrong = "f".repeat(64)
        val sentence = Pairing.pin(code.fingerprint, wrong)
        assertTrue(sentence!!.contains("fingerprint"))
        assertTrue(sentence.contains("The copy did not start."))
        assertTrue(sentence.contains(code.fingerprint))
        assertTrue(sentence.contains(wrong))
        assertFalse(sentence.contains("camera-secret"))
    }

    @Test
    fun aWrongVersionDoesNotEchoTheToken() {
        val error = assertThrows(PairingException::class.java) {
            Pairing.parse(
                "albumen-pair://v2?endpoint=https%3A%2F%2F127.0.0.1%2F&fingerprint=ab&token=${"a".repeat(64)}",
            )
        }
        assertTrue(error.message!!.contains("did not start"))
        assertFalse(error.message!!.contains("a".repeat(64)))
    }

    @Test
    fun certificateBytesPinToTheKnownDigest() {
        assertEquals(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            Pairing.fingerprint("abc".toByteArray(Charsets.UTF_8)),
        )
    }
}
