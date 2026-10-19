// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * The camera-roll sources may delete the cache spool. They may not delete or
 * update a MediaStore row, and they may not open an original for write.
 */
class CameraRollReadOnlyTest {
    @Test
    fun mainSourcesDoNotDeleteOrUpdateMediaStore() {
        val roots = listOf(
            File("src/main/kotlin"),
            File("../host/src/main/kotlin"),
        )
        val kotlin = roots.flatMap { root ->
            assertTrue(root.isDirectory)
            root.walkTopDown().filter { it.isFile && it.extension == "kt" }.toList()
        }
        assertTrue(kotlin.isNotEmpty())
        val forbiddenCall = listOf(
            Regex("""(?i)(contentResolver|resolver|getContentResolver\(\))\s*\.\s*delete\s*\("""),
            Regex("""(?i)(contentResolver|resolver|getContentResolver\(\))\s*\.\s*update\s*\("""),
            Regex("""(?i)(contentResolver|resolver|getContentResolver\(\))\s*\.\s*applyBatch\s*\("""),
            Regex("""createDeleteRequest"""),
            Regex("""createTrashRequest"""),
            Regex("""createWriteRequest"""),
            Regex("""createFavoriteRequest"""),
            Regex("""openOutputStream"""),
            Regex("""newDelete\s*\("""),
            Regex("""newUpdate\s*\("""),
            Regex("""openFileDescriptor\([^)]*"w"""),
            Regex("""openFileDescriptor\([^)]*"rw"""),
            Regex("""openFileDescriptor\([^)]*"wt"""),
            Regex("""openFileDescriptor\([^)]*"rwt"""),
        )
        for (file in kotlin) {
            file.readLines().forEachIndexed { index, raw ->
                val code = raw.substringBefore("//")
                if (".delete(" in code && "spool.file.delete()" !in code) {
                    throw AssertionError("${file.path}:${index + 1} deletes something other than the cache spool")
                }
                for (pattern in forbiddenCall) {
                    if (pattern.containsMatchIn(code)) {
                        throw AssertionError("${file.path}:${index + 1} matches ${pattern.pattern}")
                    }
                }
            }
        }
        val androidTest = File("src/androidTest/kotlin")
        if (androidTest.isDirectory) {
            androidTest.walkTopDown().filter { it.isFile && it.extension == "kt" }.forEach { file ->
                file.readLines().forEachIndexed { index, raw ->
                    val code = raw.substringBefore("//")
                    for (pattern in forbiddenCall.filter {
                        it.pattern.contains("delete") || it.pattern.contains("update") || it.pattern.contains("Delete") || it.pattern.contains("Trash") || it.pattern.contains("Write")
                    }) {
                        if (pattern.containsMatchIn(code)) {
                            throw AssertionError("${file.path}:${index + 1} matches ${pattern.pattern}")
                        }
                    }
                }
            }
        }
        val cameraRoll = File("src/main/kotlin/photos/albumen/backup/CameraRoll.kt").readText()
        assertTrue(cameraRoll.contains("MediaStore.setRequireOriginal"))
        assertTrue(cameraRoll.contains("openFileDescriptor(original, \"r\")"))
        assertTrue(cameraRoll.contains("ACCESS_MEDIA_LOCATION"))
        val worker = File("src/main/kotlin/photos/albumen/backup/CameraRollBackupWorker.kt").readText()
        assertTrue(worker.contains("executeBackup"))
        assertTrue(worker.contains("publishReport"))
        assertFalse(worker.contains("catch (_:"))
        assertFalse(worker.contains("workDataOf"))
    }

    @Test
    fun manifestsDoNotRequestWriteAccessToStorage() {
        val manifests = listOf(
            File("src/main/AndroidManifest.xml"),
            File("../host/src/main/AndroidManifest.xml"),
        )
        for (manifest in manifests) {
            val text = manifest.readText()
            assertFalse(text.contains("WRITE_EXTERNAL_STORAGE"))
            assertFalse(text.contains("MANAGE_EXTERNAL_STORAGE"))
            assertFalse(text.contains("MANAGE_MEDIA"))
            assertTrue(text.contains("ACCESS_MEDIA_LOCATION"))
        }
    }
}
