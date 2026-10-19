// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.io.InputStream
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.net.SocketTimeoutException
import java.util.Collections

/**
 * One-shot WebDAV stand-in for tests. It answers PUT with the status and
 * `X-Albumen-Sha256` the caller supplies. It does not write a camera roll.
 */
class LoopbackWebDav(
    private val onPut: (headers: String, body: ByteArray) -> Reply,
) : Closeable {
    data class Reply(val status: Int, val sha256: String?, val reason: String = "OK")

    private val server = ServerSocket(0)
    private val thread: Thread
    val bodies: MutableList<ByteArray> = Collections.synchronizedList(mutableListOf())
    val headerBlocks: MutableList<String> = Collections.synchronizedList(mutableListOf())
    val port: Int = server.localPort

    fun endpoint(): String = "http://127.0.0.1:$port/"

    init {
        server.soTimeout = 200
        thread = Thread({
            while (!server.isClosed) {
                val socket = try {
                    server.accept()
                } catch (_: SocketTimeoutException) {
                    continue
                } catch (_: SocketException) {
                    break
                }
                socket.soTimeout = 10_000
                try {
                    handle(socket)
                } catch (_: Exception) {
                    // The client closed after the response, or the test ended.
                } finally {
                    try {
                        socket.close()
                    } catch (_: Exception) {
                        // Already closed.
                    }
                }
            }
        }, "albumen-loopback-webdav")
        thread.isDaemon = true
        thread.start()
    }

    private fun handle(socket: Socket) {
        val input = socket.getInputStream()
        val header = readHeader(input)
        headerBlocks += header
        if (header.contains("100-continue", ignoreCase = true)) {
            socket.getOutputStream().write("HTTP/1.1 100 Continue\r\n\r\n".toByteArray(Charsets.US_ASCII))
            socket.getOutputStream().flush()
        }
        val length = contentLength(header)
        val body = ByteArray(length)
        var read = 0
        while (read < length) {
            val n = input.read(body, read, length - read)
            if (n < 0) {
                break
            }
            read += n
        }
        val exact = if (read == length) body else body.copyOf(read)
        bodies += exact
        val reply = onPut(header, exact)
        val reason = reply.reason.replace('\r', ' ').replace('\n', ' ')
        val shaLine = if (reply.sha256 == null) "" else "X-Albumen-Sha256: ${reply.sha256}\r\n"
        val response = "HTTP/1.1 ${reply.status} $reason\r\nContent-Length: 0\r\nConnection: close\r\n$shaLine\r\n"
        socket.getOutputStream().write(response.toByteArray(Charsets.US_ASCII))
        socket.getOutputStream().flush()
    }

    override fun close() {
        server.close()
        thread.join(2_000)
    }

    companion object {
        fun readHeader(input: InputStream): String {
            val buf = ByteArrayOutputStream()
            var matched = 0
            val needle = byteArrayOf(13, 10, 13, 10)
            while (buf.size() < 64 * 1024) {
                val value = input.read()
                if (value < 0) {
                    break
                }
                buf.write(value)
                if (value.toByte() == needle[matched]) {
                    matched++
                    if (matched == needle.size) {
                        break
                    }
                } else {
                    matched = if (value.toByte() == needle[0]) 1 else 0
                }
            }
            // String(ByteArray, Charset) is on API 30. ByteArrayOutputStream.toString(Charset) is not.
            return String(buf.toByteArray(), Charsets.ISO_8859_1)
        }

        fun contentLength(header: String): Int {
            val match = Regex("(?i)content-length:\\s*(\\d+)").find(header)
                ?: throw IllegalArgumentException("PUT had no Content-Length")
            return match.groupValues[1].toInt()
        }
    }
}
