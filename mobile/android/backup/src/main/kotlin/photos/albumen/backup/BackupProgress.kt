// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import java.io.File
import java.io.FileOutputStream

/**
 * Progress cache for a backup that was interrupted. The camera roll stays the
 * source of truth. Dropping this file uploads again, and the receiver dedupes
 * identical bytes. The file stores a content URI, size, and modified time. It
 * does not store pixels or GPS.
 */
interface ProgressStore {
    fun isVerified(key: String): Boolean
    fun markVerified(key: String)
}

fun progressKey(item: RollItem): String = "${item.uri}\t${item.sizeBytes}\t${item.mtimeSeconds}"

class FileProgressStore(private val file: File) : ProgressStore {
    private val verified = HashSet<String>()

    init {
        if (file.isFile) {
            file.useLines { lines ->
                lines.forEach { line ->
                    val key = line.trim()
                    if (key.isNotEmpty()) {
                        verified.add(key)
                    }
                }
            }
        }
    }

    override fun isVerified(key: String): Boolean = synchronized(verified) { key in verified }

    override fun markVerified(key: String) {
        synchronized(verified) {
            if (!verified.add(key)) {
                return
            }
            file.parentFile?.mkdirs()
            FileOutputStream(file, true).use { out ->
                out.write((key + "\n").toByteArray(Charsets.UTF_8))
                out.fd.sync()
            }
        }
    }
}

/** In-memory progress for tests. The worker uses [FileProgressStore]. */
class MemoryProgressStore : ProgressStore {
    private val verified = HashSet<String>()

    override fun isVerified(key: String): Boolean = synchronized(verified) { key in verified }

    override fun markVerified(key: String) {
        synchronized(verified) { verified.add(key) }
    }
}
