// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.content.Context
import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption

/**
 * The status screen reads this file. A new status is written to a temp file
 * in the same directory, fsynced, and renamed over the previous one. The
 * previous file is not removed first.
 */
object BackupStatusStore {
    const val FILE_NAME = "albumen-backup-status.txt"

    fun write(context: Context, text: String) {
        write(context.filesDir, text)
    }

    fun write(dir: File, text: String) {
        check(text.isNotBlank()) { "backup status is empty" }
        dir.mkdirs()
        val target = File(dir, FILE_NAME)
        val tmp = File(dir, "$FILE_NAME.tmp")
        FileOutputStream(tmp).use { out ->
            out.write(text.toByteArray(Charsets.UTF_8))
            out.fd.sync()
        }
        Files.move(
            tmp.toPath(),
            target.toPath(),
            StandardCopyOption.REPLACE_EXISTING,
            StandardCopyOption.ATOMIC_MOVE,
        )
    }

    fun read(context: Context): String = read(context.filesDir)

    fun read(dir: File): String {
        val target = File(dir, FILE_NAME)
        return if (target.isFile) target.readText(Charsets.UTF_8) else ""
    }
}
