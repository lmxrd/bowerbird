package eu.vardalabs.organizer.core.upload

import okhttp3.MediaType
import okhttp3.RequestBody
import okio.BufferedSink
import okio.buffer
import okio.source
import java.io.File
import java.io.IOException
import java.io.InputStream

/** What is uploaded: a name, a length known up front, and its bytes from any offset. */
interface UploadSource {
    val name: String
    val length: Long

    /** The bytes from [offset] on. */
    fun open(offset: Long): InputStream
}

/** A file on disk. */
class FileUploadSource(
    private val file: File,
    override val name: String = file.name,
) : UploadSource {
    override val length: Long get() = file.length()

    override fun open(offset: Long): InputStream = file.inputStream().also { it.skipFully(offset) }
}

/** Told the bytes sent so far of the whole. */
fun interface UploadProgress {
    fun onProgress(
        sent: Long,
        total: Long,
    )
}

/** Skip exactly [count] bytes, or fail: `skip` may skip fewer. */
fun InputStream.skipFully(count: Long) {
    var left = count
    while (left > 0) {
        val skipped = skip(left)
        if (skipped > 0) {
            left -= skipped
        } else if (read() == -1) {
            throw IOException("The file is shorter than expected")
        } else {
            left--
        }
    }
}

/**
 * [length] bytes of a source from [offset], streamed (never held in
 * memory), with the progress of the whole upload.
 */
internal class SourceBody(
    private val source: UploadSource,
    private val offset: Long,
    private val length: Long,
    private val type: MediaType?,
    private val progress: UploadProgress,
) : RequestBody() {
    override fun contentType(): MediaType? = type

    override fun contentLength(): Long = length

    override fun writeTo(sink: BufferedSink) {
        source.open(offset).source().buffer().use { input ->
            var sent = 0L
            while (sent < length) {
                val read = input.read(sink.buffer, minOf(SEGMENT, length - sent))
                if (read == -1L) throw IOException("The file is shorter than expected")
                sent += read
                sink.emit()
                progress.onProgress(offset + sent, source.length)
            }
        }
    }

    private companion object {
        const val SEGMENT = 64L * 1024
    }
}
