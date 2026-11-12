package eu.vardalabs.organizer.core.upload

import eu.vardalabs.organizer.core.api.ApiException
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlin.random.Random

class UploadClientTest {
    private lateinit var server: MockWebServer
    private val fake = FakeUploadServer()
    private val folder = UUID.randomUUID()
    private val resume = MemoryResumeStore()
    private val pauses = mutableListOf<Long>()

    // No silent retries: the client's own resumption is under test.
    private val http = OkHttpClient.Builder().retryOnConnectionFailure(false).build()

    @Before
    fun start() {
        server = MockWebServer()
        server.dispatcher = fake
        server.start()
    }

    @After
    fun stop() {
        server.close()
    }

    private fun client(maxAttempts: Int = 5) =
        UploadClient(
            http,
            server.url("/"),
            chunkSize = 300,
            plainLimit = 100,
            maxAttempts = maxAttempts,
            sleep = { pauses += it },
        )

    private fun source(
        size: Int,
        name: String = "video.mp4",
    ): Pair<UploadSource, ByteArray> {
        val bytes = Random(size).nextBytes(size)
        val file = File.createTempFile("upload", ".bin").apply { deleteOnExit() }
        file.writeBytes(bytes)
        return FileUploadSource(file, name) to bytes
    }

    @Test
    fun `a small file goes in one plain multipart request`() {
        val (src, _) = source(50, "note.txt")
        val node = client().upload(folder, src, resume, "k")
        assertEquals("note.txt", node!!.name)
        val request = fake.requests.single()
        assertEquals("/api/files/$folder/files", request.url.encodedPath)
        assertTrue(request.headers["Content-Type"]!!.startsWith("multipart/form-data"))
    }

    @Test
    fun `a large file is created, then appended in chunks at the offsets`() {
        val (src, bytes) = source(1000)
        val sent = mutableListOf<Long>()
        val node =
            client().upload(folder, src, resume, "k") { done, total ->
                sent += done
                assertEquals(1000, total)
            }

        assertEquals("video.mp4", node!!.name)
        assertArrayEquals(bytes, fake.placed!!.second)
        val create = fake.requests.first()
        assertEquals("POST", create.method)
        assertEquals(folder.toString(), create.url.queryParameter("parent_id"))
        assertEquals("?0", create.headers["Upload-Complete"])
        assertEquals("1000", create.headers["Upload-Length"])
        val patches = fake.requests.filter { it.method == "PATCH" }
        assertEquals(listOf("0", "300", "600", "900"), patches.map { it.headers["Upload-Offset"] })
        assertEquals(listOf("?0", "?0", "?0", "?1"), patches.map { it.headers["Upload-Complete"] })
        assertTrue(patches.all { it.headers["Content-Type"] == "application/partial-upload" })
        assertTrue(fake.requests.all { it.headers["Upload-Draft-Interop-Version"] == "6" })
        assertEquals(1000L, sent.last())
        assertNull("Forgotten once complete", resume.get("k"))
    }

    @Test
    fun `a dropped connection asks the offset and goes on from there`() {
        val (src, bytes) = source(1000)
        fake.dropNextAppendAfter = 120
        client().upload(folder, src, resume, "k")

        assertArrayEquals(bytes, fake.placed!!.second)
        val methods = fake.requests.map { it.method + " " + (it.headers["Upload-Offset"] ?: "") }
        assertEquals("PATCH 0", methods[1])
        assertEquals("HEAD ", methods[2])
        assertEquals("PATCH 120", methods[3])
        assertEquals(listOf(1_000L), pauses)
    }

    @Test
    fun `an upload kept from before goes on without being created again`() {
        val (src, bytes) = source(1000)
        fake.dropNextAppendAfter = 250
        try {
            client(maxAttempts = 1).upload(folder, src, resume, "k")
            fail("Expected the dropped connection")
        } catch (_: IOException) {
        }
        assertNotNull("Kept for the next try", resume.get("k"))
        fake.requests.clear()

        client().upload(folder, src, resume, "k")
        assertArrayEquals(bytes, fake.placed!!.second)
        assertEquals("HEAD", fake.requests.first().method)
        assertTrue(fake.requests.none { it.method == "POST" })
        assertEquals("250", fake.requests[1].headers["Upload-Offset"])
    }

    @Test
    fun `an upload the server no longer has starts again`() {
        val (src, bytes) = source(1000)
        resume.put("k", server.url("/api/uploads/expired").toString())
        client().upload(folder, src, resume, "k")
        assertEquals(listOf("HEAD", "POST"), fake.requests.take(2).map { it.method })
        assertArrayEquals(bytes, fake.placed!!.second)
    }

    @Test
    fun `an offset conflict goes on from the server's offset`() {
        val (src, bytes) = source(1000)
        val created = client().create(folder, "video.mp4", 1000)
        client().append(created.location, src, 0, 300, complete = false)
        val answer = client().append(created.location, src, 0, 300, complete = false)
        assertEquals(UploadClient.Appended.Conflict(300), answer)
        resume.put("k", created.location.toString())
        client().upload(folder, src, resume, "k")
        assertArrayEquals(bytes, fake.placed!!.second)
    }

    @Test
    fun `a final answer that is lost is a finished upload`() {
        val (src, bytes) = source(1000)
        fake.loseFinalAnswer = true
        val node = client().upload(folder, src, resume, "k")
        assertNull(node)
        assertArrayEquals(bytes, fake.placed!!.second)
        assertNull(resume.get("k"))
    }

    @Test
    fun `a taken name gets a number`() {
        val (src, _) = source(1000, "photo.jpg")
        fake.taken += "photo.jpg"
        val node = client().upload(folder, src, resume, "k")
        assertEquals("photo (2).jpg", node!!.name)

        val (small, _) = source(10, "a.txt")
        fake.taken += "a.txt"
        assertEquals("a (2).txt", client().upload(folder, small, resume, "s")!!.name)
    }

    @Test
    fun `an error of the request is not retried`() {
        val (src, _) = source(1000)
        val missing = UUID.randomUUID()
        try {
            UploadClient(http, server.url("/nowhere/"), plainLimit = 100, sleep = { pauses += it })
                .uploadResumable(missing, src, "video.mp4", MemoryResumeStore(), "k")
            fail("Expected a 404")
        } catch (e: ApiException) {
            assertEquals(404, e.status)
        }
        assertTrue(pauses.isEmpty())
    }

    @Test
    fun `retries stop after the last attempt`() {
        val (src, _) = source(1000)
        server.close()
        try {
            client(maxAttempts = 3).upload(folder, src, resume, "k")
            fail("Expected the connection to fail")
        } catch (_: IOException) {
        }
        assertEquals(listOf(1_000L, 2_000L), pauses)
    }

    @Test
    fun `a file for the drop box carries its note and options first`() {
        val (src, _) = source(10, "receipt.pdf")
        val items = client().dropFile(src, DropOptions(note = " tax 2025 ", readContents = false))
        assertEquals("receipt.pdf", items.single().name)
        val request = fake.requests.single()
        assertEquals("/api/drop-box/files", request.url.encodedPath)
        val body = request.body!!.utf8()
        assertTrue(body.indexOf("name=\"note\"") < body.indexOf("name=\"file\""))
        assertTrue(body.contains("tax 2025"))
        assertTrue(Regex("name=\"read_contents\"[^-]*false").containsMatchIn(body))
    }

    @Test
    fun `a small file for the drop box goes plainly`() {
        val (src, _) = source(10, "receipt.pdf")
        val item = client().drop(src, DropOptions(), resume, "k")
        assertEquals("receipt.pdf", item!!.name)
        val request = fake.requests.single()
        assertEquals("/api/drop-box/files", request.url.encodedPath)
    }

    @Test
    fun `a large file for the drop box goes resumably with its options`() {
        val (src, bytes) = source(1000, "scan 1+2.pdf")
        fake.dropNextAppendAfter = 120
        val options = DropOptions(note = " tax & 2025 ", readContents = false)
        val item = client().drop(src, options, resume, "k")
        assertEquals("scan 1+2.pdf", item!!.name)
        assertArrayEquals(bytes, fake.placed!!.second)
        assertNull(resume.get("k"))
        val created = fake.requests.first()
        assertEquals("POST", created.method)
        assertEquals("/api/uploads", created.url.encodedPath)
        assertEquals("true", created.url.queryParameter("drop_box"))
        assertEquals("scan 1+2.pdf", created.url.queryParameter("name"))
        assertEquals("tax & 2025", created.url.queryParameter("note"))
        assertEquals("false", created.url.queryParameter("read_contents"))
        assertNull(created.url.queryParameter("parent_id"))
        // The break was resumed from the server's offset.
        assertTrue(fake.requests.any { it.method == "HEAD" })
        assertTrue(fake.requests.none { it.url.encodedPath == "/api/drop-box/files" })
    }

    @Test
    fun `a drop-box upload without options leaves them to the settings`() {
        val (src, _) = source(1000, "a.bin")
        client().drop(src, DropOptions(note = "  "), resume, "k")
        val query = fake.requests.first().url
        assertEquals(setOf("drop_box", "name"), query.queryParameterNames)
    }

    @Test
    fun `names are numbered before the extension`() {
        assertEquals("a.txt", UploadClient.numbered("a.txt", 1))
        assertEquals("a (3).txt", UploadClient.numbered("a.txt", 3))
        assertEquals("README (2)", UploadClient.numbered("README", 2))
        assertEquals(".env (2)", UploadClient.numbered(".env", 2))
    }
}
