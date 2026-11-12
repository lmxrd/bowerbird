package eu.vardalabs.organizer.upload

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.core.net.toUri
import eu.vardalabs.organizer.core.upload.FileUploadSource
import eu.vardalabs.organizer.core.upload.UploadSource
import eu.vardalabs.organizer.core.upload.skipFully
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID

/** Something picked or shared, ready for an upload that may run after the screen is gone. */
data class Prepared(
    val source: String,
    val name: String,
    val size: Long,
    val staged: Boolean,
)

/**
 * Makes picked and shared content readable by the upload worker later: a
 * share's URI access ends with the activity that received it, so a URI
 * whose access cannot be kept (made persistable) is copied into the app's
 * own storage first, and deleted once uploaded.
 */
class Staging(
    private val context: Context,
) {
    private val dir get() = File(context.filesDir, "staged")

    fun prepare(uri: Uri): Prepared {
        val (name, size) = describe(uri)
        val persisted =
            try {
                context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                true
            } catch (_: SecurityException) {
                false
            }
        if (persisted && size >= 0) return Prepared(uri.toString(), name, size, staged = false)
        val copy = File(File(dir, UUID.randomUUID().toString()).apply { mkdirs() }, name)
        context.contentResolver.openInputStream(uri).use { input ->
            requireNotNull(input) { "Cannot read $name" }
            copy.outputStream().use { input.copyTo(it) }
        }
        return Prepared(copy.path, name, copy.length(), staged = true)
    }

    /** Text (a note shared, or a link to place as a file) as a file of the app's own. */
    fun prepareText(
        name: String,
        text: String,
    ): Prepared {
        val file = File(File(dir, UUID.randomUUID().toString()).apply { mkdirs() }, name)
        file.writeText(text)
        return Prepared(file.path, name, file.length(), staged = true)
    }

    /**
     * [prepared]'s bytes sealed by [encrypt] into a file of the app's own
     * (for an encrypted folder: the upload sends that copy), then the plain
     * one released.
     */
    fun seal(
        prepared: Prepared,
        encrypt: (InputStream, OutputStream) -> Unit,
    ): Prepared {
        val copy = File(File(dir, UUID.randomUUID().toString()).apply { mkdirs() }, "encrypted")
        var sealed = false
        try {
            open(prepared.source, prepared.staged).use { input ->
                copy.outputStream().buffered().use { encrypt(input, it) }
            }
            sealed = true
        } finally {
            if (!sealed) copy.parentFile?.deleteRecursively()
        }
        release(prepared.source, prepared.staged)
        return Prepared(copy.path, prepared.name, copy.length(), staged = true)
    }

    private fun open(
        source: String,
        staged: Boolean,
    ): InputStream =
        if (staged) {
            File(source).inputStream()
        } else {
            requireNotNull(context.contentResolver.openInputStream(source.toUri())) { "Cannot read the file" }
        }

    /** The bytes of a request's source. */
    fun source(request: UploadRequest): UploadSource {
        val path = requireNotNull(request.source) { "No file" }
        return if (request.staged) {
            FileUploadSource(File(path), request.name)
        } else {
            UriSource(context, path.toUri(), request.name, request.size)
        }
    }

    /** Done with a request's source: the copy goes, a kept URI access is given back. */
    fun release(request: UploadRequest) {
        release(request.source ?: return, request.staged)
    }

    private fun release(
        path: String,
        staged: Boolean,
    ) {
        if (staged) {
            File(path).parentFile?.takeIf { it.parentFile == dir }?.deleteRecursively()
        } else {
            runCatching {
                context.contentResolver.releasePersistableUriPermission(
                    path.toUri(),
                    Intent.FLAG_GRANT_READ_URI_PERMISSION,
                )
            }
        }
    }

    /** The name a content URI shows, or null when it cannot be read. */
    fun nameOf(uri: Uri): String? = runCatching { describe(uri).first }.getOrNull()

    private fun describe(uri: Uri): Pair<String, Long> {
        var name: String? = null
        var size = -1L
        context.contentResolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)
            ?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (nameIndex >= 0) name = cursor.getString(nameIndex)
                    if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) size = cursor.getLong(sizeIndex)
                }
            }
        return fileName(name ?: uri.lastPathSegment) to size
    }

    companion object {
        /** A name the server takes: no slashes, not empty. */
        fun fileName(raw: String?): String =
            raw
                ?.substringAfterLast('/')
                ?.replace(Regex("[\\u0000-\\u001f\\\\]"), "")
                ?.trim()
                ?.take(255)
                ?.ifEmpty { null } ?: "Shared file"
    }
}

/** A content URI the app may read, by its length known up front. */
private class UriSource(
    private val context: Context,
    private val uri: Uri,
    override val name: String,
    override val length: Long,
) : UploadSource {
    override fun open(offset: Long): InputStream {
        val input = context.contentResolver.openInputStream(uri) ?: error("Cannot read $name")
        input.skipFully(offset)
        return input
    }
}
