// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.content.ContentResolver
import android.content.ContentUris
import android.Manifest
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import java.io.File
import java.io.InputStream
import java.util.UUID

data class RollItem(val uri: Uri, val displayName: String)

data class Spool(val file: File, val sha256: String, val filename: String)

/**
 * Read camera-roll originals. `setRequireOriginal` with `openFileDescriptor`
 * is the call that returns the file MediaStore has, including a motion photo's
 * embedded video. `openInputStream` is not used: the original flag is defined
 * for `openFileDescriptor`. GPS stays only when the host app also holds
 * `ACCESS_MEDIA_LOCATION`.
 */
object CameraRoll {
    fun requiredPermissions(): Array<String> = arrayOf(
        Manifest.permission.READ_MEDIA_IMAGES,
        Manifest.permission.READ_MEDIA_VIDEO,
        Manifest.permission.ACCESS_MEDIA_LOCATION,
        Manifest.permission.READ_EXTERNAL_STORAGE,
    )

    fun list(resolver: ContentResolver): List<RollItem> {
        val collection = MediaStore.Files.getContentUri(MediaStore.VOLUME_EXTERNAL)
        val projection = arrayOf(
            MediaStore.Files.FileColumns._ID,
            MediaStore.Files.FileColumns.DISPLAY_NAME,
        )
        val selection =
            "${MediaStore.Files.FileColumns.MEDIA_TYPE}=? OR ${MediaStore.Files.FileColumns.MEDIA_TYPE}=?"
        val args = arrayOf(
            MediaStore.Files.FileColumns.MEDIA_TYPE_IMAGE.toString(),
            MediaStore.Files.FileColumns.MEDIA_TYPE_VIDEO.toString(),
        )
        val items = mutableListOf<RollItem>()
        resolver.query(collection, projection, selection, args, null)?.use { cursor ->
            val idColumn = cursor.getColumnIndexOrThrow(MediaStore.Files.FileColumns._ID)
            val nameColumn = cursor.getColumnIndexOrThrow(MediaStore.Files.FileColumns.DISPLAY_NAME)
            while (cursor.moveToNext()) {
                val name = cursor.getString(nameColumn) ?: continue
                val uri = ContentUris.withAppendedId(collection, cursor.getLong(idColumn))
                items += RollItem(uri, name)
            }
        }
        return items
    }

    fun spool(resolver: ContentResolver, item: RollItem, cacheDir: File): Spool {
        val name = StoredName.require(item.displayName)
        val original = MediaStore.setRequireOriginal(item.uri)
        val descriptor = resolver.openFileDescriptor(original, "r")
            ?: throw java.io.IOException("original unavailable for $name")
        return ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
            copyToCache(input, name, cacheDir)
        }
    }
}

internal fun copyToCache(input: InputStream, filename: String, cacheDir: File): Spool {
    val dir = File(cacheDir, "albumen-spool").apply { mkdirs() }
    val out = File(dir, "${UUID.randomUUID()}-$filename")
    val digest = java.security.MessageDigest.getInstance("SHA-256")
    out.outputStream().use { output ->
        val buf = ByteArray(1024 * 1024)
        while (true) {
            val n = input.read(buf)
            if (n < 0) {
                break
            }
            if (n == 0) {
                continue
            }
            digest.update(buf, 0, n)
            output.write(buf, 0, n)
        }
        output.fd.sync()
    }
    val sha256 = digest.digest().joinToString("") { "%02x".format(it) }
    return Spool(out, sha256, filename)
}
