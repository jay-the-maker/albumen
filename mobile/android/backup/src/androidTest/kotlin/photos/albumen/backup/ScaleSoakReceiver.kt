// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.io.Closeable
import java.io.InputStream
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.net.SocketTimeoutException
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference

/**
 * Loopback receiver for the scale soak. A path that leaves the caller's
 * filename, including `members/`, is refused and not stored. Drop and
 * power-loss close the socket after the first 64 body bytes.
 */
class ScaleSoakReceiver(
    private val fault: AtomicReference<String>,
) : Closeable {
    private val server = ServerSocket(0)
    private val thread: Thread
    val port: Int = server.localPort
    val paths: MutableList<String> = Collections.synchronizedList(mutableListOf())
    val stored: MutableMap<String, ByteArray> = Collections.synchronizedMap(linkedMapOf())

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
                socket.soTimeout = 20_000
                try {
                    handle(socket)
                } catch (_: Exception) {
                    // The injected fault closed the socket, or the client did.
                } finally {
                    try {
                        socket.close()
                    } catch (_: Exception) {
                        // Already closed.
                    }
                }
            }
        }, "albumen-scale-soak-receiver")
        thread.isDaemon = true
        thread.start()
    }

    private fun handle(socket: Socket) {
        val input = socket.getInputStream()
        val header = LoopbackWebDav.readHeader(input)
        if (header.contains("100-continue", ignoreCase = true)) {
            socket.getOutputStream().write("HTTP/1.1 100 Continue\r\n\r\n".toByteArray(Charsets.US_ASCII))
            socket.getOutputStream().flush()
        }
        val path = header.lineSequence().firstOrNull()?.split(" ")?.getOrNull(1).orEmpty()
        paths += path
        val length = LoopbackWebDav.contentLength(header)
        if (path.contains("..") || path.contains("/members/") || path.startsWith("/members/")) {
            discard(input, length)
            val refused = "HTTP/1.1 403 refusing a write outside this item\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            socket.getOutputStream().write(refused.toByteArray(Charsets.US_ASCII))
            socket.getOutputStream().flush()
            return
        }
        when (fault.get()) {
            "network-drop" -> closeShort(socket, input, length, kill = false)
            "power-loss" -> closeShort(socket, input, length, kill = true)
            "sha-mismatch" -> respond(socket, input, length, path, sha = MISMATCH_SHA)
            else -> respond(socket, input, length, path, sha = null)
        }
    }

    private fun closeShort(socket: Socket, input: InputStream, length: Int, kill: Boolean) {
        val want = minOf(length, SHORT_READ).coerceAtLeast(0)
        var got = 0
        val buf = ByteArray(256)
        while (got < want) {
            val n = input.read(buf, 0, minOf(buf.size, want - got))
            if (n < 0) {
                break
            }
            got += n
        }
        check(length > SHORT_READ) { "scale soak item must be longer than the short read" }
        check(got < length) { "scale soak fault read the whole body ($got of $length)" }
        if (kill) {
            onPowerLoss?.invoke()
        }
        socket.close()
    }

    var onPowerLoss: (() -> Unit)? = null

    private fun discard(input: InputStream, length: Int) {
        var left = length
        val buf = ByteArray(1024)
        while (left > 0) {
            val n = input.read(buf, 0, minOf(buf.size, left))
            if (n < 0) {
                break
            }
            left -= n
        }
    }

    private fun respond(socket: Socket, input: InputStream, length: Int, path: String, sha: String?) {
        val body = ByteArray(length)
        var read = 0
        while (read < length) {
            val n = input.read(body, read, length - read)
            if (n < 0) {
                break
            }
            read += n
        }
        check(read == length) { "scale soak receiver got $read of $length body bytes" }
        stored[path] = body
        val digest = sha ?: Sha256.hex(body)
        val response = "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\nX-Albumen-Sha256: $digest\r\n\r\n"
        socket.getOutputStream().write(response.toByteArray(Charsets.US_ASCII))
        socket.getOutputStream().flush()
    }

    override fun close() {
        server.close()
        thread.join(2_000)
    }

    companion object {
        const val MISMATCH_SHA = "0000000000000000000000000000000000000000000000000000000000000000"
        private const val SHORT_READ = 64

        fun closedPort(): Int = ServerSocket(0).use { it.localPort }
    }
}
