package eu.vardalabs.organizer.upload

import eu.vardalabs.organizer.ROOT
import eu.vardalabs.organizer.core.e2e.E2e
import eu.vardalabs.organizer.core.e2e.Keyring
import eu.vardalabs.organizer.data.Folder
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.file.Files
import java.util.UUID

class EncryptedUploadsTest {
    private val dir: File = Files.createTempDirectory("staged").toFile()
    private val folder = UUID.randomUUID().let { Folder(it, "Private", "/Private", ROOT.id, e2eRoot = it) }
    private val key = E2e.random(32)
    private val ring = Keyring(folder.id, 2, mapOf(2 to key))
    private val fresh = mutableListOf<Boolean>()

    @After
    fun cleanUp() {
        dir.deleteRecursively()
    }

    /** As Staging's: the plain file sealed into another, the plain one gone. */
    private fun seal(
        prepared: Prepared,
        encrypt: (java.io.InputStream, java.io.OutputStream) -> Unit,
    ): Prepared {
        val plain = File(prepared.source)
        val copy = File(dir, "${UUID.randomUUID()}.enc")
        plain.inputStream().use { input -> copy.outputStream().use { encrypt(input, it) } }
        plain.delete()
        return Prepared(copy.path, prepared.name, copy.length(), staged = true)
    }

    private fun plain(
        name: String,
        bytes: ByteArray,
    ): Prepared {
        val file = File(dir, UUID.randomUUID().toString()).apply { writeBytes(bytes) }
        return Prepared(file.path, name, file.length(), staged = true)
    }

    @Test
    fun `names are made free and sealed, contents sealed with the newest key`() =
        runTest {
            val uploads =
                EncryptedUploads(
                    keyring = { root, isFresh ->
                        assertEquals(folder.id, root)
                        fresh += isFresh
                        ring
                    },
                    names = { listOf("Photo.jpg", "notes.txt") },
                    seal = ::seal,
                )
            val bytes = ByteArray(70_000) { (it % 251).toByte() }
            val requests =
                uploads.requests(
                    folder,
                    listOf(plain("photo.jpg", bytes), plain("photo.jpg", ByteArray(0)), plain("a.txt", bytes)),
                )

            assertEquals(listOf(true), fresh)
            assertEquals(listOf("photo (2).jpg", "photo (3).jpg", "a.txt"), requests.map { it.name })
            requests.forEach { request ->
                assertEquals(UploadKind.FOLDER, request.kind)
                assertEquals(folder.id.toString(), request.folderId)
                assertTrue(request.staged)
                assertEquals(request.name, ring.decryptName(folder.id, request.encryptedName!!))
                assertEquals(File(request.source!!).length(), request.size)
            }
            val opened = ByteArrayOutputStream()
            File(requests.first().source!!).inputStream().use { ring.decrypt(it, opened) }
            assertArrayEquals(bytes, opened.toByteArray())
        }

    @Test
    fun `into a folder from another server, names are sealed with its id there`() =
        runTest {
            val remote = folder.copy(remote = "b.example")
            val theirs = UUID.randomUUID()
            val uploads =
                EncryptedUploads(
                    keyring = { _, _ -> ring },
                    names = { emptyList() },
                    seal = ::seal,
                    sealingId = { if (it.remote != null) theirs else it.id },
                )
            val request = uploads.requests(remote, listOf(plain("a.txt", ByteArray(3)))).single()
            assertEquals(remote.id.toString(), request.folderId)
            assertEquals("a.txt", ring.decryptName(theirs, request.encryptedName!!))
        }
}
