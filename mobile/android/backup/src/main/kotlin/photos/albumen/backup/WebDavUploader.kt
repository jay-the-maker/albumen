// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.net.Uri
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.Base64

/** PUT an original at `albumen-receive`. The camera-roll row is not this type's concern. */
class WebDavUploader(
    private val endpoint: String,
    private val user: String,
    private val password: String,
) {
    fun put(file: File, filename: String, clientSha256: String): UploadVerdict {
        val name = StoredName.require(filename)
        val connection = (endpointUrl(endpoint, name).openConnection() as HttpURLConnection)
        try {
            connection.requestMethod = "PUT"
            connection.instanceFollowRedirects = false
            connection.doOutput = true
            connection.setFixedLengthStreamingMode(file.length())
            connection.setRequestProperty("Content-Type", "application/octet-stream")
            val token = Base64.getEncoder().encodeToString("$user:$password".toByteArray(Charsets.UTF_8))
            connection.setRequestProperty("Authorization", "Basic $token")
            file.inputStream().use { input ->
                connection.outputStream.use { output -> input.copyTo(output) }
            }
            return UploadCheck.verdict(
                clientSha256,
                connection.responseCode,
                connection.getHeaderField("X-Albumen-Sha256"),
            )
        } finally {
            connection.disconnect()
        }
    }
}

internal fun endpointUrl(endpoint: String, filename: String): URL {
    val base = if (endpoint.endsWith("/")) endpoint else "$endpoint/"
    return URL(base + Uri.encode(filename))
}
