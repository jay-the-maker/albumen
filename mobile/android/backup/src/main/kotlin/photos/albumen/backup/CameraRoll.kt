// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.Manifest
import android.content.ContentResolver
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import androidx.core.content.ContextCompat
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.util.UUID

data class RollItem(
    val uri: Uri,
    val displayName: String,
    val sizeBytes: Long,
    val mtimeSeconds: Long,
)

data class Spool(val file: File, val sha256: String, val filename: String)

/**
 * Read camera-roll originals. `setRequireOriginal` with `openFileDescriptor`
 * in mode `"r"` is the call that returns the file MediaStore has, including a
 * motion photo's embedded video and its GPS. `openInputStream` is not used.
 * This object does not show a permission dialog and does not write MediaStore.
 */
object CameraRoll {
    const val LOCATION_BLOCK =
        "Location access is missing, so MediaStore would strip GPS. Backup did not read the camera roll and is not finished."
    const val MEDIA_BLOCK =
        "Images or video access is missing. Backup did not read the camera roll."

    /**
     * Runtime permissions the host app requests before enqueueing backup.
     * API 33 and newer use the media permissions. API 30 through 32 use
     * `READ_EXTERNAL_STORAGE`. Location is always requested with them.
     * This library has no Activity, so it does not show the dialog.
     */
    fun requiredPermissions(sdkInt: Int = Build.VERSION.SDK_INT): Array<String> {
        val media = if (sdkInt >= 33) {
            listOf(
                Manifest.permission.READ_MEDIA_IMAGES,
                Manifest.permission.READ_MEDIA_VIDEO,
            )
        } else {
            listOf(Manifest.permission.READ_EXTERNAL_STORAGE)
        }
        return (media + Manifest.permission.ACCESS_MEDIA_LOCATION).toTypedArray()
    }

    /**
     * A missing location grant means MediaStore would strip GPS, so the backup
     * must not open the file. A missing media grant is the same refusal.
     */
    fun permissionBlock(context: Context, sdkInt: Int = Build.VERSION.SDK_INT): String? {
        val denied = requiredPermissions(sdkInt).filter { permission ->
            ContextCompat.checkSelfPermission(context, permission) != PackageManager.PERMISSION_GRANTED
        }
        if (denied.isEmpty()) {
            return null
        }
        if (Manifest.permission.ACCESS_MEDIA_LOCATION in denied) {
            return LOCATION_BLOCK
        }
        return MEDIA_BLOCK
    }

    /**
     * One page of images and videos, newest `DATE_ADDED` first. [limit] is a
     * page size, not a cap on the camera roll. The caller increases [offset]
     * until a short page comes back.
     */
    fun page(resolver: ContentResolver, offset: Int, limit: Int): List<RollItem> {
        require(offset >= 0) { "page offset is negative" }
        require(limit > 0) { "page size is empty" }
        val collection = MediaStore.Files.getContentUri(MediaStore.VOLUME_EXTERNAL)
        val projection = arrayOf(
            MediaStore.Files.FileColumns._ID,
            MediaStore.Files.FileColumns.DISPLAY_NAME,
            MediaStore.Files.FileColumns.SIZE,
            MediaStore.Files.FileColumns.DATE_MODIFIED,
        )
        val selection =
            "${MediaStore.Files.FileColumns.MEDIA_TYPE}=? OR ${MediaStore.Files.FileColumns.MEDIA_TYPE}=?"
        val args = arrayOf(
            MediaStore.Files.FileColumns.MEDIA_TYPE_IMAGE.toString(),
            MediaStore.Files.FileColumns.MEDIA_TYPE_VIDEO.toString(),
        )
        val queryArgs = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
            putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, args)
            putString(
                ContentResolver.QUERY_ARG_SQL_SORT_ORDER,
                "${MediaStore.Files.FileColumns.DATE_ADDED} DESC",
            )
            putInt(ContentResolver.QUERY_ARG_LIMIT, limit)
            putInt(ContentResolver.QUERY_ARG_OFFSET, offset)
        }
        val cursor = resolver.query(collection, projection, queryArgs, null)
            ?: throw IOException("MediaStore query failed")
        val items = ArrayList<RollItem>(limit)
        cursor.use { rows ->
            val idColumn = rows.getColumnIndexOrThrow(MediaStore.Files.FileColumns._ID)
            val nameColumn = rows.getColumnIndexOrThrow(MediaStore.Files.FileColumns.DISPLAY_NAME)
            val sizeColumn = rows.getColumnIndexOrThrow(MediaStore.Files.FileColumns.SIZE)
            val mtimeColumn = rows.getColumnIndexOrThrow(MediaStore.Files.FileColumns.DATE_MODIFIED)
            while (rows.moveToNext()) {
                val name = if (rows.isNull(nameColumn)) "" else rows.getString(nameColumn).orEmpty()
                val size = if (rows.isNull(sizeColumn)) -1L else rows.getLong(sizeColumn)
                val mtime = if (rows.isNull(mtimeColumn)) -1L else rows.getLong(mtimeColumn)
                val uri = android.content.ContentUris.withAppendedId(collection, rows.getLong(idColumn))
                items += RollItem(uri, name, size, mtime)
            }
        }
        return items
    }

    fun spool(resolver: ContentResolver, item: RollItem, cacheDir: File): Spool {
        val name = StoredName.require(item.displayName)
        if (item.mtimeSeconds < 0) {
            throw IOException("no modified time for $name")
        }
        val original = MediaStore.setRequireOriginal(item.uri)
        val descriptor = resolver.openFileDescriptor(original, "r")
            ?: throw IOException("original unavailable for $name")
        return ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
            val spool = copyToCache(input, name, cacheDir)
            spool.file.setLastModified(item.mtimeSeconds * 1000L)
            spool
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
