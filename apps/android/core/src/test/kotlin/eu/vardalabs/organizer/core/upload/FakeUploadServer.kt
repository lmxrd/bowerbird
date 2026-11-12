package eu.vardalabs.organizer.core.upload

import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.RecordedRequest
import mockwebserver3.SocketEffect
import okio.Buffer
import java.util.UUID

/**
 * A stand-in for `routes/uploads.rs` and the plain upload: it keeps the
 * uploads in memory and can be told to drop a connection, keeping part of
 * the bytes, as a real dropped connection would.
 */
class FakeUploadServer : Dispatcher() {
    class Upload(
        val name: String,
        val length: Long?,
        val dropBox: Boolean = false,
    ) {
        val bytes = Buffer()
    }

    val uploads = mutableMapOf<String, Upload>()
    val requests = mutableListOf<RecordedRequest>()
    val taken = mutableSetOf<String>()
    var placed: Pair<String, ByteArray>? = null

    /** For the next PATCH: keep this many of its bytes, then drop the connection. */
    var dropNextAppendAfter: Long? = null

    /** For the next PATCH that completes the upload: place it, then drop the connection. */
    var loseFinalAnswer = false

    @Synchronized
    override fun dispatch(request: RecordedRequest): MockResponse {
        requests += request
        val path = request.url.encodedPath
        return when {
            request.method == "POST" && path == "/api/uploads" -> {
                create(request)
            }

            request.method == "HEAD" && path.startsWith("/api/uploads/") -> {
                head(path.substringAfterLast('/'))
            }

            request.method == "PATCH" &&
                path.startsWith(
                    "/api/uploads/",
                )
            -> {
                append(request, path.substringAfterLast('/'))
            }

            request.method == "POST" && path.endsWith("/files") -> {
                plain(request)
            }

            else -> {
                MockResponse
                    .Builder()
                    .code(404)
                    .body("""{"error":"Not found"}""")
                    .build()
            }
        }
    }

    private fun create(request: RecordedRequest): MockResponse {
        val name = request.url.queryParameter("name")!!
        if (name in taken) {
            return MockResponse
                .Builder()
                .code(409)
                .body("""{"error":"The name is taken"}""")
                .build()
        }
        val id = UUID.randomUUID().toString()
        val dropBox = request.url.queryParameter("drop_box") == "true"
        uploads[id] = Upload(name, request.headers["Upload-Length"]?.toLong(), dropBox)
        return MockResponse
            .Builder()
            .code(201)
            .addHeader("Location", "/api/uploads/$id")
            .addHeader("Upload-Offset", "0")
            .addHeader("Upload-Complete", "?0")
            .build()
    }

    private fun head(id: String): MockResponse {
        val upload = uploads[id] ?: return MockResponse.Builder().code(404).build()
        return MockResponse
            .Builder()
            .code(204)
            .addHeader("Upload-Offset", upload.bytes.size)
            .build()
    }

    private fun append(
        request: RecordedRequest,
        id: String,
    ): MockResponse {
        val upload = uploads[id] ?: return MockResponse.Builder().code(404).build()
        val at = request.headers["Upload-Offset"]!!.toLong()
        if (at != upload.bytes.size) {
            return MockResponse
                .Builder()
                .code(409)
                .addHeader("Upload-Offset", upload.bytes.size)
                .body("""{"error":"Another offset"}""")
                .build()
        }
        val body = request.body!!.toByteArray()
        dropNextAppendAfter?.let { keep ->
            dropNextAppendAfter = null
            upload.bytes.write(body, 0, keep.toInt())
            return MockResponse.Builder().onResponseStart(SocketEffect.ShutdownConnection).build()
        }
        upload.bytes.write(body)
        val complete = request.headers["Upload-Complete"] == "?1"
        if (!complete) {
            return MockResponse
                .Builder()
                .code(204)
                .addHeader("Upload-Offset", upload.bytes.size)
                .build()
        }
        uploads.remove(id)
        placed = upload.name to upload.bytes.readByteArray()
        if (loseFinalAnswer) {
            loseFinalAnswer = false
            return MockResponse.Builder().onResponseStart(SocketEffect.ShutdownConnection).build()
        }
        return MockResponse
            .Builder()
            .code(201)
            .body(if (upload.dropBox) item(upload.name) else node(upload.name))
            .build()
    }

    private fun plain(request: RecordedRequest): MockResponse {
        val text = request.body!!.utf8()
        val name = Regex("filename=\"([^\"]+)\"").find(text)!!.groupValues[1]
        if (name in taken) {
            return MockResponse
                .Builder()
                .code(409)
                .body("""{"error":"The name is taken"}""")
                .build()
        }
        return MockResponse
            .Builder()
            .code(201)
            .body(if (request.url.encodedPath.startsWith("/api/drop-box")) "[${item(name)}]" else node(name))
            .build()
    }

    companion object {
        fun node(name: String) =
            """{"id":"${UUID.randomUUID()}","drive_id":"${UUID.randomUUID()}",""" +
                """"parent_id":"${UUID.randomUUID()}","name":"$name","kind":"file",""" +
                """"size":1,"mime":null,"version":1,"portable":true,"created_at":"t","updated_at":"t"}"""

        fun item(name: String) =
            """{"id":"${UUID.randomUUID()}","kind":"file","name":"$name","read_contents":true,""" +
                """"status":"queued","created_at":"t","updated_at":"t"}"""
    }
}
