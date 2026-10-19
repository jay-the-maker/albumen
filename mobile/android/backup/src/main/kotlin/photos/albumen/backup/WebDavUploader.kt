// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.Base64

/** PUT an original at `albumen-receive`. The camera-roll row is not this type's concern. */
class WebDavUploader(
    private val endpoint: String,
    private val user: String,
    private val password: String,
    private val memberId: String = "",
) : OriginalUploader {
    override fun put(file: File, filename: String, clientSha256: String, mtimeSeconds: Long): UploadVerdict {
        val name = StoredName.require(filename)
        val relative = if (memberId.isEmpty()) {
            encodePathSegment(name)
        } else {
            MemberNamespace.relativePath(endpoint, memberId, name)
        }
        val connection = (joinEndpoint(endpoint, relative).openConnection() as HttpURLConnection)
        try {
            connection.requestMethod = "PUT"
            connection.instanceFollowRedirects = false
            connection.doOutput = true
            connection.connectTimeout = CONNECT_TIMEOUT_MS
            connection.readTimeout = 0
            connection.setFixedLengthStreamingMode(file.length())
            connection.setRequestProperty("Content-Type", "application/octet-stream")
            connection.setRequestProperty("X-Albumen-Mtime", mtimeSeconds.toString())
            val token = Base64.getEncoder().encodeToString("$user:$password".toByteArray(Charsets.UTF_8))
            connection.setRequestProperty("Authorization", "Basic $token")
            file.inputStream().use { input ->
                connection.outputStream.use { output -> input.copyTo(output) }
            }
            val status = connection.responseCode
            val serverHash = connection.getHeaderField("X-Albumen-Sha256")
            val body = if (status >= 400) connection.errorStream else connection.inputStream
            body?.close()
            return UploadCheck.verdict(clientSha256, status, serverHash)
        } finally {
            connection.disconnect()
        }
    }

    private companion object {
        /** A dead receiver fails this quickly. The body itself has no read timeout, so a large video is not a size cap. */
        const val CONNECT_TIMEOUT_MS = 30_000
    }
}

fun interface OriginalUploader {
    fun put(file: File, filename: String, clientSha256: String, mtimeSeconds: Long): UploadVerdict
}

/** Unix seconds for `X-Albumen-Mtime`. Milliseconds from a file clock are truncated. */
internal fun sourceMtimeSeconds(lastModifiedMillis: Long): String =
    (lastModifiedMillis / 1_000L).toString()

internal fun endpointUrl(endpoint: String, filename: String): URL =
    joinEndpoint(endpoint, encodePathSegment(filename))

internal fun joinEndpoint(endpoint: String, relativePath: String): URL {
    val base = if (endpoint.endsWith("/")) endpoint else "$endpoint/"
    return URL(base + relativePath)
}

/**
 * A phone backup in a family library writes `members/<ed25519-pubkey>/<filename>`
 * and nothing else. The endpoint is the library root. A path that is not that
 * prefix is refused before the request is sent.
 */
object MemberNamespace {
    fun canonicalMemberId(raw: String): String {
        val id = raw.trim().lowercase()
        if (id.length != 64 || id.any { it !in '0'..'9' && it !in 'a'..'f' }) {
            throw IllegalArgumentException("member id is not a 32-byte Ed25519 public key")
        }
        return id
    }

    fun relativePath(endpoint: String, memberId: String, filename: String): String {
        val path = URL(endpoint).path.trim('/')
        if (path.isNotEmpty()) {
            throw IllegalArgumentException("backup endpoint is not the family library root")
        }
        val id = canonicalMemberId(memberId)
        val name = StoredName.require(filename)
        return "members/$id/${encodePathSegment(name)}"
    }
}

/** Percent-encode one camera filename. Spaces are `%20`. `/` is not left in the segment. */
internal fun encodePathSegment(name: String): String = buildString {
    for (byte in name.toByteArray(Charsets.UTF_8)) {
        val value = byte.toInt() and 0xFF
        val ch = value.toChar()
        if (ch.isLetterOrDigit() || ch == '-' || ch == '.' || ch == '_' || ch == '~') {
            append(ch)
        } else {
            append('%')
            append("0123456789ABCDEF"[value ushr 4])
            append("0123456789ABCDEF"[value and 0x0F])
        }
    }
}
