// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.security.MessageDigest

/**
 * `albumen-pair://v1` from `albumen-receive --pair`.
 *
 * The URI carries the WebDAV endpoint, the SHA-256 fingerprint of the
 * receiver certificate, a one-time token, and the family member public key
 * when the receiver was started for one member. The same vectors are parsed
 * in `receive/src/pairing.rs` and on iOS.
 */
data class PairingCode(
    val endpoint: String,
    val fingerprint: String,
    val token: String,
    val member: String?,
)

object Pairing {
    const val EXAMPLE_WITH_MEMBER =
        "albumen-pair://v1?endpoint=https%3A%2F%2F127.0.0.1%3A8443%2F&fingerprint=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&member=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

    const val EXAMPLE_WITHOUT_MEMBER =
        "albumen-pair://v1?endpoint=https%3A%2F%2Fphotos.example%3A8443%2F&fingerprint=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

    fun parse(text: String): PairingCode {
        val trimmed = text.trim()
        if (trimmed.length > 2048 || !trimmed.startsWith("albumen-pair://")) {
            throw PairingException(REJECTED)
        }
        val rest = trimmed.removePrefix("albumen-pair://")
        val split = rest.indexOf('?')
        if (split < 0) {
            throw PairingException(REJECTED)
        }
        val version = rest.substring(0, split).trimEnd('/')
        if (version != "v1") {
            throw PairingException(REJECTED)
        }
        val pairs = queryPairs(rest.substring(split + 1))
        val endpoint = required(pairs, "endpoint")
        val fingerprint = normalizeFingerprint(required(pairs, "fingerprint")) ?: throw PairingException(REJECTED)
        val token = required(pairs, "token")
        if (!isHex64(token) || !validEndpoint(endpoint)) {
            throw PairingException(REJECTED)
        }
        val member = when (val value = pairs["member"]) {
            null -> null
            else -> {
                if (!isHex64(value)) throw PairingException(REJECTED)
                value.lowercase()
            }
        }
        return PairingCode(endpoint, fingerprint, token.lowercase(), member)
    }

    /** SHA-256 of the certificate encoding, lowercase hex. */
    fun fingerprint(encodedCertificate: ByteArray): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(encodedCertificate)
        return digest.joinToString("") { byte -> "%02x".format(byte) }
    }

    /**
     * Null when the certificate matches the pairing code.
     * A mismatch is the sentence shown on the status screen. The receiver
     * password is not part of that sentence, and the copy does not start.
     */
    fun pin(expected: String, presented: String): String? {
        val presentedClean = presented.trim().replace("\r", " ").replace("\n", " ")
        val expectedNorm = normalizeFingerprint(expected)
        val presentedNorm = normalizeFingerprint(presentedClean)
        if (expectedNorm != null && expectedNorm == presentedNorm) {
            return null
        }
        if (expectedNorm == null) {
            return "Certificate fingerprint does not match the pairing code. The pairing code has no fingerprint. The copy did not start."
        }
        return "Certificate fingerprint does not match the pairing code. Expected $expectedNorm. Received $presentedClean. The copy did not start."
    }

    private const val REJECTED =
        "The pairing code is not an albumen-pair v1 URI. The copy did not start."

    private fun queryPairs(query: String): Map<String, String> {
        if (query.isEmpty()) throw PairingException(REJECTED)
        val pairs = linkedMapOf<String, String>()
        for (part in query.split('&')) {
            val eq = part.indexOf('=')
            if (eq <= 0) throw PairingException(REJECTED)
            val key = percentDecode(part.substring(0, eq))
            val value = percentDecode(part.substring(eq + 1))
            if (pairs.containsKey(key)) throw PairingException(REJECTED)
            pairs[key] = value
        }
        return pairs
    }

    private fun required(pairs: Map<String, String>, key: String): String =
        pairs[key] ?: throw PairingException(REJECTED)

    private fun validEndpoint(endpoint: String): Boolean {
        if (!endpoint.startsWith("https://")) return false
        val rest = endpoint.removePrefix("https://")
        if (rest.isEmpty() || rest.any { it == ' ' || it == '\n' || it == '\r' || it == '@' }) return false
        return rest.substringBefore('/').isNotEmpty()
    }

    private fun normalizeFingerprint(value: String): String? {
        val trimmed = value.trim()
        return if (isHex64(trimmed)) trimmed.lowercase() else null
    }

    private fun isHex64(value: String): Boolean =
        value.length == 64 && value.all { it.isDigit() || it in 'a'..'f' || it in 'A'..'F' }

    private fun percentDecode(text: String): String {
        val bytes = ArrayList<Byte>(text.length)
        var index = 0
        while (index < text.length) {
            if (text[index] == '%') {
                if (index + 2 >= text.length) throw PairingException(REJECTED)
                val hex = text.substring(index + 1, index + 3)
                val value = hex.toIntOrNull(16) ?: throw PairingException(REJECTED)
                bytes.add(value.toByte())
                index += 3
            } else {
                val encoded = text[index].toString().toByteArray(Charsets.UTF_8)
                bytes.addAll(encoded.toList())
                index += 1
            }
        }
        return bytes.toByteArray().toString(Charsets.UTF_8)
    }
}

class PairingException(message: String) : IllegalArgumentException(message)
