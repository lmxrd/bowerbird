package eu.vardalabs.organizer.core.e2e

import eu.vardalabs.organizer.api.models.EncryptionKeys
import eu.vardalabs.organizer.api.models.EncryptionTask
import eu.vardalabs.organizer.api.models.FolderKeys
import eu.vardalabs.organizer.api.models.Member
import eu.vardalabs.organizer.api.models.MemberKey
import eu.vardalabs.organizer.api.models.PassphraseWrap
import eu.vardalabs.organizer.api.models.WrappedKey
import eu.vardalabs.organizer.core.api.ApiException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.UUID

/**
 * A user of the fake server: their key pair, or none before they set up
 * encryption; on another server, their address ([remote]) and [id] the
 * share's.
 */
class TestUser(
    val id: UUID = UUID.randomUUID(),
    val private: ByteArray? = E2e.random(32),
    val remote: String? = null,
) {
    val public: ByteArray? get() = private?.let(E2e::publicKey)
}

/** An encrypted folder as the server keeps it: its members' wrapped keys of each generation. */
class TestFolder(
    val id: UUID,
    val members: List<TestUser>,
    var generation: Int = 0,
    var needsNewKey: Boolean = generation == 0,
) {
    val wrapped = mutableMapOf<UUID, MutableMap<Int, String>>()

    /** Make a generation and wrap it for [to], as another device would. */
    fun addGeneration(to: List<TestUser> = members.filter { it.public != null }): ByteArray {
        generation++
        val key = E2e.random(32)
        to.forEach { give(it, generation, key) }
        needsNewKey = false
        return key
    }

    fun give(
        user: TestUser,
        generation: Int,
        key: ByteArray,
    ) {
        wrapped.getOrPut(user.id) { mutableMapOf() }[generation] =
            E2e.b64(E2e.wrapFolderKey(key, id, generation, user.public!!))
    }
}

/** The server's part, in memory, as `routes/encryption.rs` answers [me]. */
class FakeEncryptionServer(
    val me: TestUser,
    passphrase: String,
    val iterations: Long = 100_000,
) : EncryptionServer {
    private val salt = E2e.random(16)
    private val wrapped = E2e.wrapPrivate(E2e.passphraseKey(passphrase, salt, iterations.toInt()), me.private!!)
    val folders = mutableMapOf<UUID, TestFolder>()
    val posted = mutableListOf<List<MemberKey>>()
    var conflictOnce = false

    /** Shared from another server: the ids here that answer for the owner's folder there. */
    val copies = mutableMapOf<UUID, UUID>()

    /** The owner's server does not answer. */
    var unreachable = false

    override suspend fun keys() =
        EncryptionKeys(
            setUp = true,
            publicKey = E2e.b64(me.public!!),
            passphrase = PassphraseWrap(iterations = iterations, salt = E2e.b64(salt), wrapped = E2e.b64(wrapped)),
            recoveryWrapped = null,
        )

    override suspend fun folderKeys(id: UUID): FolderKeys {
        if (unreachable) throw ApiException(502, "no answer", ApiException.REMOTE_UNREACHABLE)
        val folder = folders[copies[id] ?: id] ?: throw ApiException(404, "Not found")
        return FolderKeys(
            folderId = folder.id,
            generation = folder.generation.toLong(),
            needsNewKey = folder.needsNewKey,
            propertyKeys =
                folder.wrapped[me.id]
                    .orEmpty()
                    .map { (g, w) -> WrappedKey(generation = g.toLong(), wrapped = w) },
            members =
                folder.members.map { user ->
                    val have = folder.wrapped[user.id].orEmpty().keys
                    Member(
                        email = "${user.id}@example.com",
                        missing =
                            if (user.public == null) {
                                emptyList()
                            } else {
                                (1..folder.generation).filter { it !in have }.map { it.toLong() }
                            },
                        publicKey = user.public?.let(E2e::b64),
                        userId = user.id,
                        remote = user.remote,
                    )
                },
        )
    }

    override suspend fun addFolderKeys(
        id: UUID,
        keys: List<MemberKey>,
    ): FolderKeys {
        if (conflictOnce) {
            conflictOnce = false
            throw ApiException(409, "Another device made it")
        }
        val folder = folders.getValue(id)
        posted += keys
        if (keys.any { it.generation.toInt() == folder.generation + 1 }) {
            folder.generation++
            folder.needsNewKey = false
        }
        keys.forEach { folder.wrapped.getOrPut(it.userId) { mutableMapOf() }[it.generation.toInt()] = it.wrapped }
        return folderKeys(id)
    }

    override suspend fun pending(): List<EncryptionTask> =
        folders.values.mapNotNull { folder ->
            val keys = folderKeys(folder.id)
            val waiting = keys.members.filter { it.missing.isNotEmpty() }.map { it.userId }
            if (folder.needsNewKey || waiting.isNotEmpty()) {
                EncryptionTask(folderId = folder.id, needsNewKey = folder.needsNewKey, waiting = waiting)
            } else {
                null
            }
        }
}

class MemoryPrivateKeyStore : PrivateKeyStore {
    var key: ByteArray? = null

    override suspend fun loadPrivateKey() = key

    override suspend fun savePrivateKey(key: ByteArray) {
        this.key = key
    }

    override suspend fun clearPrivateKey() {
        key = null
    }
}

class E2eKeysTest {
    private val me = TestUser()
    private val server = FakeEncryptionServer(me, "correct horse")
    private val store = MemoryPrivateKeyStore()

    private suspend inline fun <reified T : Throwable> assertFails(block: suspend () -> Unit) {
        try {
            block()
            fail("Expected ${T::class.simpleName}")
        } catch (e: Throwable) {
            if (e !is T) throw e
        }
    }

    @Test
    fun `a wrong passphrase leaves it locked`() =
        runTest {
            val keys = E2eKeys({ server }, store, backgroundScope)
            assertFails<WrongPassphraseException> { keys.unlock("wrong horse") }
            assertFalse(keys.isUnlocked())
            assertNull(store.key)
            assertFails<LockedException> { keys.keyring(UUID.randomUUID()) }
        }

    @Test
    fun `too few iterations are refused`() =
        runTest {
            val weak = FakeEncryptionServer(me, "correct horse", iterations = 1_000)
            val keys = E2eKeys({ weak }, store, backgroundScope)
            assertFails<E2eException> { keys.unlock("correct horse") }
            assertFalse(keys.isUnlocked())
        }

    @Test
    fun `unlocked, the key is kept and opens the folder's names until locked`() =
        runTest {
            val folder = TestFolder(UUID.randomUUID(), listOf(me))
            val key = folder.addGeneration()
            server.folders[folder.id] = folder
            val keys = E2eKeys({ server }, store, backgroundScope)
            keys.unlock("correct horse")
            assertTrue(keys.unlocked.value)
            assertArrayEquals(me.private, store.key)

            val name = E2e.encryptName(key, 1, folder.id, "Taxes.pdf")
            assertEquals("Taxes.pdf", keys.keyring(folder.id).decryptName(folder.id, name))

            // Another start reads the kept key.
            val again = E2eKeys({ server }, store, backgroundScope)
            assertTrue(again.isUnlocked())
            again.lock()
            assertFalse(again.unlocked.value)
            assertNull(store.key)
            assertFails<LockedException> { again.keyring(folder.id) }
        }

    @Test
    fun `a folder that needs a new key gets the next generation for every member with a public key`() =
        runTest {
            val other = TestUser()
            val waiting = TestUser(private = null)
            val folder = TestFolder(UUID.randomUUID(), listOf(me, other, waiting))
            folder.addGeneration()
            folder.needsNewKey = true
            server.folders[folder.id] = folder
            store.key = me.private
            val keys = E2eKeys({ server }, store, backgroundScope)

            val ring = keys.keyring(folder.id)
            assertEquals(2, ring.generation)
            assertEquals(
                setOf(me.id, other.id),
                server.posted
                    .single()
                    .map { it.userId }
                    .toSet(),
            )
            assertTrue(server.posted.single().all { it.generation == 2L })
            // What it wraps opens for the other member, and is the key it uses.
            val theirs =
                E2e.unwrapFolderKey(
                    E2e.unb64(folder.wrapped.getValue(other.id).getValue(2)),
                    folder.id,
                    2,
                    other.private!!,
                )
            assertArrayEquals(ring.key(2), theirs)
            assertEquals("a.txt", keys.keyring(folder.id).decryptName(folder.id, ring.encryptName(folder.id, "a.txt")))
        }

    @Test
    fun `a member on another server is wrapped for by the share's id, in a new generation too`() =
        runTest {
            val bob = TestUser(remote = "bob@b.example")
            val eve = TestUser(private = null, remote = "eve@c.example")
            val folder = TestFolder(UUID.randomUUID(), listOf(me, bob, eve))
            val first = folder.addGeneration(to = listOf(me))
            folder.needsNewKey = true
            server.folders[folder.id] = folder
            store.key = me.private
            val keys = E2eKeys({ server }, store, backgroundScope)

            val ring = keys.keyring(folder.id)
            assertEquals(2, ring.generation)
            val theirs = folder.wrapped.getValue(bob.id)
            assertArrayEquals(first, E2e.unwrapFolderKey(E2e.unb64(theirs.getValue(1)), folder.id, 1, bob.private!!))
            assertArrayEquals(
                ring.key(2),
                E2e.unwrapFolderKey(E2e.unb64(theirs.getValue(2)), folder.id, 2, bob.private),
            )
            // Not set up there yet: nothing for them.
            assertFalse(eve.id in folder.wrapped)
        }

    @Test
    fun `a member's device unwraps a folder from another server with the owner's folder id`() =
        runTest {
            val owners = TestFolder(UUID.randomUUID(), listOf(me))
            val key = owners.addGeneration()
            server.folders[owners.id] = owners
            val here = UUID.randomUUID()
            server.copies[here] = owners.id
            store.key = me.private
            val keys = E2eKeys({ server }, store, backgroundScope)

            // Asked by its id here; its names were sealed with the owner's ids.
            val parent = UUID.randomUUID()
            val name = E2e.encryptName(key, 1, parent, "Taxes.pdf")
            assertEquals("Taxes.pdf", keys.keyring(here).decryptName(parent, name))
            assertTrue(server.posted.isEmpty())

            server.unreachable = true
            assertFails<OwnerUnreachableException> { keys.keyring(here, fresh = true) }
        }

    @Test
    fun `when another device made the key first, it reads the folder again`() =
        runTest {
            val folder = TestFolder(UUID.randomUUID(), listOf(me))
            server.folders[folder.id] = folder
            server.conflictOnce = true
            store.key = me.private
            val keys = E2eKeys({ server }, store, backgroundScope)
            assertEquals(0, keys.keyring(folder.id).generation)
            assertTrue(server.posted.isEmpty())
        }

    @Test
    fun `members waiting get the generations they lack, from the pending list after unlocking`() =
        runTest {
            val late = TestUser()
            val folder = TestFolder(UUID.randomUUID(), listOf(me, late))
            val first = folder.addGeneration(to = listOf(me))
            val second = folder.addGeneration(to = listOf(me))
            server.folders[folder.id] = folder
            val keys = E2eKeys({ server }, store, backgroundScope)

            keys.unlock("correct horse")
            keys.seeToPending()

            val theirs = folder.wrapped.getValue(late.id)
            assertArrayEquals(first, E2e.unwrapFolderKey(E2e.unb64(theirs.getValue(1)), folder.id, 1, late.private!!))
            assertArrayEquals(second, E2e.unwrapFolderKey(E2e.unb64(theirs.getValue(2)), folder.id, 2, late.private!!))
            assertTrue(server.pending().isEmpty())
        }
}
