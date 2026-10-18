// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.io.InputStream
import java.security.MessageDigest

sealed class UploadVerdict {
    /**
     * 201 with the client's digest, including a collision-safe path for a
     * different file that shared the camera filename. 409 with the same digest
     * means those bytes were already stored and the retry was deduped.
     */
    data class Verified(val sha256: String) : UploadVerdict()
    data class Conflict(val client: String, val server: String) : UploadVerdict()
    data object MissingServerHash : UploadVerdict()
    data class Rejected(val status: Int) : UploadVerdict()
}

object UploadCheck {
    /**
     * Accept the upload only when `X-Albumen-Sha256` equals the digest of the
     * bytes the client sent. 201 is a new object, even when the camera filename
     * collided and the server stored it under another name. 409 is success only
     * when that header is the digest of the identical bytes already stored.
     */
    fun verdict(clientSha256: String, status: Int, serverSha256: String?): UploadVerdict {
        val client = clientSha256.trim().lowercase()
        val server = serverSha256?.trim()?.lowercase().orEmpty()
        if (server.isEmpty()) {
            return UploadVerdict.MissingServerHash
        }
        if (status == 201 || status == 409) {
            return if (client == server) {
                UploadVerdict.Verified(client)
            } else {
                UploadVerdict.Conflict(client, server)
            }
        }
        return UploadVerdict.Rejected(status)
    }
}

object Sha256 {
    fun hex(bytes: ByteArray): String = bytes.inputStream().use { hex(it) }

    fun hex(input: InputStream): String {
        val digest = MessageDigest.getInstance("SHA-256")
        val buf = ByteArray(1024 * 1024)
        while (true) {
            val n = input.read(buf)
            if (n < 0) {
                break
            }
            if (n > 0) {
                digest.update(buf, 0, n)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }
}

object StoredName {
    /** Keep the filename. Directory pieces are dropped. `..` is refused. */
    fun require(raw: String): String {
        val trimmed = raw.trim()
        if (trimmed.isEmpty() || trimmed.contains('\u0000') || trimmed.any { it.isISOControl() }) {
            throw IllegalArgumentException("refusing filename")
        }
        val parts = trimmed.split('/', '\\')
        if (parts.any { it == ".." }) {
            throw IllegalArgumentException("refusing a path that climbs out of the library")
        }
        return parts.lastOrNull { it.isNotEmpty() && it != "." }
            ?: throw IllegalArgumentException("missing filename")
    }
}
