package eu.vardalabs.organizer.core.e2e

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.MessageDigest
import java.util.UUID

/**
 * The format against the reference's test vectors
 * (`packages/e2e-crypto/vectors.json`, docs/E2E_SPEC.md §9), every section.
 */
class E2eVectorsTest {
    private val vectors: JsonObject =
        Json.parseToJsonElement(File(System.getProperty("e2e.vectors")!!).readText()).jsonObject

    private fun section(name: String): List<JsonObject> {
        val list = vectors.getValue(name).jsonArray
        assertTrue("$name has vectors", list.isNotEmpty())
        return list.map { it.jsonObject }
    }

    private fun JsonObject.hex(key: String): ByteArray = unhex(getValue(key).jsonPrimitive.content)

    private fun JsonObject.text(key: String): String = getValue(key).jsonPrimitive.content

    private fun JsonObject.int(key: String): Int = getValue(key).jsonPrimitive.int

    private fun JsonObject.long(key: String): Long = getValue(key).jsonPrimitive.long

    private inline fun <reified T : Throwable> assertFails(block: () -> Unit) {
        try {
            block()
            fail("Expected ${T::class.simpleName}")
        } catch (e: Throwable) {
            if (e !is T) throw e
        }
    }

    @Test
    fun `the format version is 1`() {
        assertEquals(1, vectors.getValue("format_version").jsonPrimitive.int)
    }

    @Test
    fun identities() {
        for (v in section("identities")) {
            assertArrayEquals(v.hex("public"), E2e.publicKey(v.hex("private")))
        }
    }

    @Test
    fun passphrases() {
        for (v in section("passphrases")) {
            val key = E2e.passphraseKey(v.text("passphrase"), v.hex("salt"), v.int("iterations"))
            assertArrayEquals(v.hex("key"), key)
            assertArrayEquals(v.hex("wrapped"), E2e.wrapPrivate(key, v.hex("private"), v.hex("nonce")))
            val private =
                E2e.unwrapWithPassphrase(
                    v.hex("wrapped"),
                    v.text("passphrase"),
                    v.hex("salt"),
                    v.long("iterations"),
                )
            assertArrayEquals(v.hex("private"), private)
            assertFails<DecryptException> {
                E2e.unwrapWithPassphrase(
                    v.hex("wrapped"),
                    v.text("passphrase") + "!",
                    v.hex("salt"),
                    v.long("iterations"),
                )
            }
            assertFails<E2eException> {
                E2e.unwrapWithPassphrase(
                    v.hex("wrapped"),
                    v.text("passphrase"),
                    v.hex("salt"),
                    1_000,
                )
            }
        }
    }

    @Test
    fun `a passphrase is the same in any Unicode form`() {
        val salt = ByteArray(16) { 7 }
        assertArrayEquals(E2e.passphraseKey("été", salt, 1_000), E2e.passphraseKey("été", salt, 1_000))
    }

    @Test
    fun `recovery keys`() {
        for (v in section("recovery_keys")) {
            val key = v.hex("key")
            assertEquals(v.text("text"), E2e.encodeRecovery(key))
            assertArrayEquals(key, E2e.decodeRecovery(v.text("text")))
            assertArrayEquals(key, E2e.decodeRecovery(v.text("text").lowercase().replace("-", " ")))
            assertArrayEquals(v.hex("wrapping_key"), E2e.recoveryWrappingKey(key))
            assertArrayEquals(
                v.hex("wrapped"),
                E2e.wrapPrivate(E2e.recoveryWrappingKey(key), v.hex("private"), v.hex("nonce")),
            )
            assertArrayEquals(v.hex("private"), E2e.unwrapWithRecovery(v.hex("wrapped"), key))

            val text = v.text("text").replace("-", "")
            assertFails<E2eException> { E2e.decodeRecovery(text.dropLast(1)) }
            // The last character carries one bit and four zero bits: only A or Q.
            assertFails<E2eException> { E2e.decodeRecovery(text.dropLast(1) + "B") }
            assertFails<E2eException> { E2e.decodeRecovery("1" + text.drop(1)) }
            assertFails<DecryptException> { E2e.unwrapWithRecovery(v.hex("wrapped"), E2e.random(32)) }
        }
    }

    @Test
    fun `folder keys`() {
        for (v in section("folder_keys")) {
            val folder = UUID.fromString(v.text("folder_id"))
            val generation = v.int("generation")
            assertArrayEquals(v.hex("ephemeral_public"), E2e.publicKey(v.hex("ephemeral_private")))
            val wrapped =
                E2e.wrapFolderKey(
                    v.hex("folder_key"),
                    folder,
                    generation,
                    v.hex("recipient_public"),
                    v.hex("ephemeral_private"),
                    v.hex("nonce"),
                )
            assertArrayEquals(v.hex("wrapped"), wrapped)
            assertEquals(v.text("wrapped_base64url"), E2e.b64(wrapped))
            assertArrayEquals(
                v.hex("folder_key"),
                E2e.unwrapFolderKey(wrapped, folder, generation, v.hex("recipient_private")),
            )
            assertFails<DecryptException> {
                E2e.unwrapFolderKey(
                    wrapped,
                    folder,
                    generation + 1,
                    v.hex("recipient_private"),
                )
            }
            assertFails<DecryptException> {
                E2e.unwrapFolderKey(wrapped, UUID.randomUUID(), generation, v.hex("recipient_private"))
            }
        }
    }

    @Test
    fun names() {
        for (v in section("names")) {
            val parent = UUID.fromString(v.text("parent_id"))
            val folderKey = v.hex("folder_key")
            assertArrayEquals(v.hex("name_key"), E2e.nameKey(folderKey))
            val encrypted = E2e.encryptName(folderKey, v.int("generation"), parent, v.text("name"), v.hex("nonce"))
            assertEquals(v.text("encrypted"), encrypted)
            assertEquals(v.int("generation"), E2e.nameGeneration(encrypted))
            assertEquals(v.text("decrypted"), E2e.decryptName(folderKey, parent, encrypted))
            assertFails<DecryptException> { E2e.decryptName(folderKey, UUID.randomUUID(), encrypted) }
        }
    }

    @Test
    fun `names too long are refused`() {
        val key = E2e.random(32)
        val longest = "é".repeat(E2e.MAX_NAME_BYTES / 2)
        assertTrue(E2e.encryptName(key, 1, UUID.randomUUID(), longest).length <= 255)
        assertFails<E2eException> { E2e.encryptName(key, 1, UUID.randomUUID(), longest + "a") }
        assertFails<E2eException> { E2e.encryptName(key, 1, UUID.randomUUID(), "") }
    }

    @Test
    fun contents() {
        for (v in section("contents")) {
            val length = v.int("plain_length")
            val plain = ByteArray(length) { (it % 251).toByte() }
            val folderKey = v.hex("folder_key")
            val generation = v.int("generation")
            assertArrayEquals(v.hex("plain_sha256"), sha256(plain))
            assertArrayEquals(v.hex("file_key"), Contents.fileKey(folderKey, v.hex("file_nonce")))

            val sealed = ByteArrayOutputStream()
            val written =
                Contents.encrypt(
                    folderKey,
                    generation,
                    ByteArrayInputStream(plain),
                    sealed,
                    v.hex("file_nonce"),
                )
            val encrypted = sealed.toByteArray()
            assertEquals(v.long("encrypted_length"), written)
            assertEquals(v.long("encrypted_length"), encrypted.size.toLong())
            assertEquals(v.long("encrypted_length"), Contents.encryptedSize(length.toLong()))
            assertEquals(length.toLong(), Contents.plainSize(encrypted.size.toLong()))
            assertArrayEquals(v.hex("encrypted_sha256"), sha256(encrypted))
            if (v.containsKey("encrypted")) assertArrayEquals(v.hex("encrypted"), encrypted)

            val keys = { g: Int -> if (g == generation) folderKey else null }
            assertArrayEquals(plain, open(keys, encrypted))
            // Cut at the end, or by a whole segment: the last segment's flag no longer matches.
            assertFails<E2eException> { open(keys, encrypted.copyOf(encrypted.size - 1)) }
            if (length > Contents.SEGMENT) {
                assertFails<DecryptException> {
                    open(
                        keys,
                        encrypted.copyOf(Contents.HEADER_LEN + Contents.SEALED_SEGMENT),
                    )
                }
            }
            assertFails<NoKeyException> { open({ null }, encrypted) }
        }
    }

    @Test
    fun `a changed byte does not open`() {
        val key = E2e.random(32)
        val sealed = ByteArrayOutputStream()
        Contents.encrypt(key, 1, ByteArrayInputStream(ByteArray(100) { 1 }), sealed)
        val bytes = sealed.toByteArray()
        bytes[bytes.size - 20] = (bytes[bytes.size - 20] + 1).toByte()
        assertFails<DecryptException> { open({ key }, bytes) }
        assertNotEquals(null, Contents.plainSize(bytes.size.toLong()))
    }

    private fun open(
        keys: (Int) -> ByteArray?,
        encrypted: ByteArray,
    ): ByteArray {
        val out = ByteArrayOutputStream()
        Contents.decrypt(keys, ByteArrayInputStream(encrypted), out)
        return out.toByteArray()
    }

    private fun sha256(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes)

    private fun unhex(text: String) =
        ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
