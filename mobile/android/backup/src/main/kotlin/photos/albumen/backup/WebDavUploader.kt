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
) : OriginalUploader {
    override fun put(file: File, filename: String, clientSha256: String, mtimeSeconds: Long): UploadVerdict {
        val name = StoredName.require(filename)
        val connection = (endpointUrl(endpoint, name).openConnection() as HttpURLConnection)
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

internal fun endpointUrl(endpoint: String, filename: String): URL {
    val base = if (endpoint.endsWith("/")) endpoint else "$endpoint/"
    return URL(base + encodePathSegment(filename))
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
