// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.io.Closeable
import java.io.InputStream
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.net.SocketTimeoutException
import java.util.Collections
import java.util.concurrent.atomic.AtomicInteger

/**
 * Loopback WebDAV for the backup soak. Each accepted PUT follows the fault
 * the caller returns for that connection. A drop or a kill reads only the
 * start of the body and closes the socket, so the client is still inside PUT.
 * Healthy and mismatch read the body and answer. This server does not keep a
 * camera roll.
 */
class SoakReceiver(
    private val fault: () -> Fault,
    private val onKill: () -> Unit = {},
) : Closeable {
    enum class Fault {
        HEALTHY,
        DROP,
        KILL,
        MISMATCH,
    }

    private val server = ServerSocket(0)
    private val thread: Thread
    val port: Int = server.localPort
    val headerBlocks: MutableList<String> = Collections.synchronizedList(mutableListOf())
    private val shortCloses = AtomicInteger()
    private val fullResponses = AtomicInteger()

    fun endpoint(): String = "http://127.0.0.1:$port/"

    fun shortCloseCount(): Int = shortCloses.get()

    fun fullResponseCount(): Int = fullResponses.get()

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
                    // The client closed, or this connection was the injected fault.
                } finally {
                    try {
                        socket.close()
                    } catch (_: Exception) {
                        // Already closed.
                    }
                }
            }
        }, "albumen-soak-receiver")
        thread.isDaemon = true
        thread.start()
    }

    private fun handle(socket: Socket) {
        val input = socket.getInputStream()
        val header = LoopbackWebDav.readHeader(input)
        headerBlocks += header
        if (header.contains("100-continue", ignoreCase = true)) {
            socket.getOutputStream().write("HTTP/1.1 100 Continue\r\n\r\n".toByteArray(Charsets.US_ASCII))
            socket.getOutputStream().flush()
        }
        val length = LoopbackWebDav.contentLength(header)
        when (fault()) {
            Fault.DROP -> closeShort(socket, input, length, kill = false)
            Fault.KILL -> closeShort(socket, input, length, kill = true)
            Fault.HEALTHY -> respond(socket, input, length, sha = null)
            Fault.MISMATCH -> respond(socket, input, length, sha = MISMATCH_SHA)
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
        check(got < length) { "soak fault read the whole body ($got of $length)" }
        if (kill) {
            onKill()
        }
        shortCloses.incrementAndGet()
        socket.close()
    }

    private fun respond(socket: Socket, input: InputStream, length: Int, sha: String?) {
        val body = ByteArray(length)
        var read = 0
        while (read < length) {
            val n = input.read(body, read, length - read)
            if (n < 0) {
                break
            }
            read += n
        }
        check(read == length) { "soak receiver got $read of $length body bytes" }
        val digest = sha ?: Sha256.hex(body)
        val response = "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\nX-Albumen-Sha256: $digest\r\n\r\n"
        socket.getOutputStream().write(response.toByteArray(Charsets.US_ASCII))
        socket.getOutputStream().flush()
        fullResponses.incrementAndGet()
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
