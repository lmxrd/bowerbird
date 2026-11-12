package eu.vardalabs.organizer.core.upload

import eu.vardalabs.organizer.api.models.InboxItem
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.api.GeneratedJson
import eu.vardalabs.organizer.core.api.toApiException
import kotlinx.serialization.builtins.ListSerializer
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.util.UUID

/** Where an upload stands between tries: the upload's URL, kept by the caller (across restarts). */
interface ResumeStore {
    fun get(key: String): String?

    fun put(
        key: String,
        value: String,
    )

    fun remove(key: String)
}

/** A [ResumeStore] in memory, for tests and one-off uploads. */
class MemoryResumeStore : ResumeStore {
    private val values = mutableMapOf<String, String>()

    @Synchronized override fun get(key: String) = values[key]

    @Synchronized override fun put(
        key: String,
        value: String,
    ) {
        values[key] = value
    }

    @Synchronized override fun remove(key: String) {
        values.remove(key)
    }
}

/** The options of a file for the drop box. */
data class DropOptions(
    val note: String? = null,
    /** Null for the user's setting. */
    val readContents: Boolean? = null,
)

/** Where a resumable upload goes. */
sealed interface UploadTarget {
    /** A new file in a folder. */
    data class Folder(
        val id: UUID,
        val name: String,
    ) : UploadTarget

    /** A drop-box item, with its options (`drop_box=true`). */
    data class DropBox(
        val name: String,
        val options: DropOptions = DropOptions(),
    ) : UploadTarget
}

/**
 * Our own upload client (docs/M4_DESIGN.md §2): plain multipart uploads for
 * small files, and the resumable upload protocol
 * (draft-ietf-httpbis-resumable-upload, interop version 6, as
 * `routes/uploads.rs` speaks it) for large ones, into a folder or the drop
 * box: create, append at the offset, ask the offset with HEAD after a
 * failure and go on from there.
 *
 * Blocking: run it on a background thread (the worker does). [http] must
 * authenticate (`Api.http`).
 */
class UploadClient(
    private val http: OkHttpClient,
    private val server: HttpUrl,
    private val chunkSize: Long = CHUNK_SIZE,
    private val plainLimit: Long = PLAIN_LIMIT,
    private val maxAttempts: Int = 5,
    private val sleep: (Long) -> Unit = { Thread.sleep(it) },
) {
    private val json = GeneratedJson.json

    /**
     * Upload a file into a folder: plainly under [plainLimit], resumably
     * from there. A name that is taken gets a number (`photo (2).jpg`).
     * Answers the file, or null when the last bytes arrived but their
     * answer was lost (the upload is done, its file not known).
     */
    fun upload(
        folderId: UUID,
        source: UploadSource,
        resume: ResumeStore,
        key: String,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): Node? {
        var attempt = 1
        while (true) {
            val name = numbered(source.name, attempt)
            try {
                return if (source.length < plainLimit) {
                    uploadPlain(folderId, source, name, progress)
                } else {
                    uploadResumable(folderId, source, name, resume, key, progress)
                }
            } catch (e: ApiException) {
                if (e.status != 409 || attempt == MAX_NAMES) throw e
                attempt++
            }
        }
    }

    /** `POST /api/files/{id}/files`: a multipart form with the file in `file`. */
    fun uploadPlain(
        folderId: UUID,
        source: UploadSource,
        name: String = source.name,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): Node {
        val body =
            MultipartBody
                .Builder()
                .setType(MultipartBody.FORM)
                .addFormDataPart("file", name, SourceBody(source, 0, source.length, OCTET_STREAM, progress))
                .build()
        val request =
            Request
                .Builder()
                .url(url("api/files/$folderId/files"))
                .post(body)
                .build()
        return http.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw response.toApiException()
            json.decodeFromString(Node.serializer(), response.body.string())
        }
    }

    /**
     * `POST /api/drop-box/files`: the file becomes an item of the drop box,
     * with the note and options sent before it.
     */
    fun dropFile(
        source: UploadSource,
        options: DropOptions = DropOptions(),
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): List<InboxItem> {
        val form = MultipartBody.Builder().setType(MultipartBody.FORM)
        options.note?.takeIf { it.isNotBlank() }?.let { form.addFormDataPart("note", it.trim()) }
        options.readContents?.let { form.addFormDataPart("read_contents", it.toString()) }
        form.addFormDataPart("file", source.name, SourceBody(source, 0, source.length, OCTET_STREAM, progress))
        val request =
            Request
                .Builder()
                .url(url("api/drop-box/files"))
                .post(form.build())
                .build()
        return http.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw response.toApiException()
            json.decodeFromString(ListSerializer(InboxItem.serializer()), response.body.string())
        }
    }

    /**
     * A file for the drop box: plainly under [plainLimit] ([dropFile]),
     * resumably from there ([dropResumable]), so that no size is too large.
     * Answers the item, or null when the last bytes arrived but their
     * answer was lost.
     */
    fun drop(
        source: UploadSource,
        options: DropOptions,
        resume: ResumeStore,
        key: String,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): InboxItem? =
        if (source.length < plainLimit) {
            dropFile(source, options, progress).firstOrNull()
        } else {
            dropResumable(source, options, resume, key, progress)
        }

    /** Where a resumable upload was made, and how much of it the server has. */
    data class Created(
        val location: HttpUrl,
        val offset: Long,
    )

    /** What the server answered to an append. */
    sealed interface Appended {
        /** Received; waiting for more from [offset]. */
        data class Offset(
            val offset: Long,
        ) : Appended

        /** Complete: what was made (the file, or the drop-box item), as JSON. */
        data class Done(
            val body: String,
        ) : Appended

        /** The server is at another offset: go on from [offset]. */
        data class Conflict(
            val offset: Long,
        ) : Appended
    }

    /** Make a resumable upload of [length] bytes into a folder, without bytes yet. */
    fun create(
        folderId: UUID,
        name: String,
        length: Long,
    ): Created = create(UploadTarget.Folder(folderId, name), length)

    /** Make a resumable upload of [length] bytes, without bytes yet. */
    fun create(
        target: UploadTarget,
        length: Long,
    ): Created {
        val query = url("api/uploads").newBuilder()
        when (target) {
            is UploadTarget.Folder -> {
                query.addQueryParameter("parent_id", target.id.toString())
                query.addQueryParameter("name", target.name)
            }

            is UploadTarget.DropBox -> {
                query.addQueryParameter("drop_box", "true")
                query.addQueryParameter("name", target.name)
                target.options.note
                    ?.takeIf { it.isNotBlank() }
                    ?.let { query.addQueryParameter("note", it.trim()) }
                target.options.readContents?.let { query.addQueryParameter("read_contents", it.toString()) }
            }
        }
        val request =
            Request
                .Builder()
                .url(query.build())
                .header(INTEROP, INTEROP_VERSION)
                .header("Upload-Complete", "?0")
                .header("Upload-Length", length.toString())
                .post(ByteArray(0).toRequestBody(OCTET_STREAM))
                .build()
        return http.newCall(request).execute().use { response ->
            if (response.code != 201) throw response.toApiException()
            val location =
                response.header("Location")?.let { server.resolve(it) }
                    ?: throw ApiException(response.code, "The server did not say where the upload is")
            Created(location, response.offset() ?: 0)
        }
    }

    /** `HEAD`: the bytes the server has, or null when there is no such upload (any more). */
    fun offset(location: HttpUrl): Long? {
        val request =
            Request
                .Builder()
                .url(location)
                .header(INTEROP, INTEROP_VERSION)
                .head()
                .build()
        return http.newCall(request).execute().use { response ->
            when {
                response.code == 404 || response.code == 410 -> null
                response.isSuccessful -> response.offset() ?: throw ApiException(response.code, "No Upload-Offset")
                else -> throw response.toApiException()
            }
        }
    }

    /** `PATCH`: [length] bytes of [source] at [offset]; [complete] when they are the last. */
    fun append(
        location: HttpUrl,
        source: UploadSource,
        offset: Long,
        length: Long,
        complete: Boolean,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): Appended {
        val request =
            Request
                .Builder()
                .url(location)
                .header(INTEROP, INTEROP_VERSION)
                .header("Upload-Offset", offset.toString())
                .header("Upload-Complete", if (complete) "?1" else "?0")
                .patch(SourceBody(source, offset, length, PARTIAL_UPLOAD, progress))
                .build()
        return http.newCall(request).execute().use { response ->
            when (response.code) {
                201 -> Appended.Done(response.body.string())
                204 -> Appended.Offset(response.offset() ?: (offset + length))
                409 -> response.offset()?.let { Appended.Conflict(it) } ?: throw response.toApiException()
                else -> throw response.toApiException()
            }
        }
    }

    /** `DELETE`: what the server received goes. */
    fun cancel(location: HttpUrl) {
        val request =
            Request
                .Builder()
                .url(location)
                .header(INTEROP, INTEROP_VERSION)
                .delete()
                .build()
        http.newCall(request).execute().use { response ->
            if (!response.isSuccessful && response.code != 404) throw response.toApiException()
        }
    }

    /** A whole resumable upload into a folder ([resumable]): the file. */
    fun uploadResumable(
        folderId: UUID,
        source: UploadSource,
        name: String,
        resume: ResumeStore,
        key: String,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): Node? =
        resumable(UploadTarget.Folder(folderId, name), source, resume, key, progress)
            ?.let { json.decodeFromString(Node.serializer(), it) }

    /** A whole resumable upload to the drop box ([resumable]): the item it became. */
    fun dropResumable(
        source: UploadSource,
        options: DropOptions,
        resume: ResumeStore,
        key: String,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): InboxItem? =
        resumable(UploadTarget.DropBox(source.name, options), source, resume, key, progress)
            ?.let { json.decodeFromString(InboxItem.serializer(), it) }

    /**
     * A whole resumable upload, answering what was made as JSON. The
     * upload's URL is kept in [resume] under [key] as soon as it exists, so
     * a later call (after the app was killed, or the worker retried) goes
     * on where the server is. A dropped connection is retried here,
     * [maxAttempts] times in a row with a growing pause, before the error
     * is thrown to the caller.
     */
    fun resumable(
        target: UploadTarget,
        source: UploadSource,
        resume: ResumeStore,
        key: String,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ): String? {
        val length = source.length
        var failures = 0
        while (true) {
            try {
                var location = resume.get(key)?.toHttpUrlOrNull()
                var offset = 0L
                if (location != null) {
                    val known = offset(location)
                    if (known == null) {
                        // Gone: expired, or completed while its answer was lost.
                        val finalSent = resume.get(finalKey(key)) != null
                        resume.remove(key)
                        resume.remove(finalKey(key))
                        if (finalSent) return null
                        location = null
                    } else {
                        offset = known
                    }
                }
                if (location == null) {
                    val created = create(target, length)
                    location = created.location
                    offset = created.offset
                    resume.put(key, location.toString())
                }
                progress.onProgress(offset, length)
                while (true) {
                    val end = minOf(offset + chunkSize, length)
                    val complete = end == length
                    if (complete) resume.put(finalKey(key), "1")
                    when (val answer = append(location!!, source, offset, end - offset, complete, progress)) {
                        is Appended.Done -> {
                            resume.remove(key)
                            resume.remove(finalKey(key))
                            return answer.body
                        }

                        is Appended.Offset -> {
                            offset = answer.offset
                            failures = 0
                        }

                        is Appended.Conflict -> {
                            offset = answer.offset
                        }
                    }
                }
            } catch (e: IOException) {
                if (e is ApiException && !e.retryable) {
                    resume.remove(key)
                    resume.remove(finalKey(key))
                    throw e
                }
                failures++
                if (failures >= maxAttempts) throw e
                sleep(BACKOFF_MS * (1L shl (failures - 1)))
            }
        }
    }

    /** Download a file to [dest] (written to a temporary file next to it first). */
    fun download(
        nodeId: UUID,
        dest: File,
        progress: UploadProgress = UploadProgress { _, _ -> },
    ) {
        val request =
            Request
                .Builder()
                .url(url("api/files/$nodeId/content"))
                .get()
                .build()
        http.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw response.toApiException()
            val total = response.body.contentLength()
            dest.parentFile?.mkdirs()
            val partial = File(dest.path + ".part")
            response.body.byteStream().use { input ->
                partial.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024)
                    var received = 0L
                    while (true) {
                        val read = input.read(buffer)
                        if (read == -1) break
                        output.write(buffer, 0, read)
                        received += read
                        progress.onProgress(received, total)
                    }
                }
            }
            if (!partial.renameTo(dest)) throw IOException("Cannot save ${dest.name}")
        }
    }

    /** Stream a file's content to [consume] (to decrypt it on the way, for one). */
    fun read(
        nodeId: UUID,
        consume: (InputStream) -> Unit,
    ) {
        val request =
            Request
                .Builder()
                .url(url("api/files/$nodeId/content"))
                .get()
                .build()
        http.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw response.toApiException()
            response.body.byteStream().use(consume)
        }
    }

    private fun url(path: String): HttpUrl = server.newBuilder().addPathSegments(path).build()

    private fun Response.offset(): Long? = header("Upload-Offset")?.trim()?.toLongOrNull()

    private fun finalKey(key: String) = "$key.final"

    companion object {
        /** Smaller files go in one plain request. */
        const val PLAIN_LIMIT = 4L * 1024 * 1024

        /** The bytes of one append: a dropped connection loses at most this. */
        const val CHUNK_SIZE = 8L * 1024 * 1024

        const val INTEROP = "Upload-Draft-Interop-Version"
        const val INTEROP_VERSION = "6"
        private const val BACKOFF_MS = 1_000L
        private const val MAX_NAMES = 20
        private val OCTET_STREAM = "application/octet-stream".toMediaType()
        private val PARTIAL_UPLOAD = "application/partial-upload".toMediaType()

        /** `photo.jpg`, then `photo (2).jpg`, `photo (3).jpg`, ... */
        fun numbered(
            name: String,
            n: Int,
        ): String {
            if (n <= 1) return name
            val dot = name.lastIndexOf('.')
            return if (dot > 0) "${name.substring(0, dot)} ($n)${name.substring(dot)}" else "$name ($n)"
        }
    }
}
