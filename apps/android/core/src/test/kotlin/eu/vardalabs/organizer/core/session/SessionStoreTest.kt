package eu.vardalabs.organizer.core.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import okio.ByteString.Companion.encodeUtf8
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** Reverses the bytes: enough to tell that the token is not stored as it is. */
class FakeCipher : TokenCipher {
    var broken = false

    override fun encrypt(plain: ByteArray) = plain.reversedArray()

    override fun decrypt(data: ByteArray): ByteArray {
        check(!broken) { "Key lost" }
        return data.reversedArray()
    }
}

class SessionStoreTest {
    private val dir: File = Files.createTempDirectory("session").toFile()
    private val scopes = mutableListOf<CoroutineScope>()
    private val cipher = FakeCipher()

    private fun store(): SessionStore {
        val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
        scopes += scope
        val data = PreferenceDataStoreFactory.create(scope = scope) { File(dir, "session.preferences_pb") }
        return SessionStore(data, cipher)
    }

    @After
    fun cleanUp() {
        scopes.forEach { it.cancel() }
        dir.deleteRecursively()
    }

    /** One DataStore per file at a time: the previous one is closed first. */
    private suspend fun closeAll() {
        scopes.forEach { it.coroutineContext[Job]!!.cancelAndJoin() }
        scopes.clear()
    }

    private val session = Session("https://example.com", "org_secret", "org_3f9a", "Pixel")

    @Test
    fun `it starts signed out, then loads what was saved`() =
        runTest {
            val first = store()
            assertEquals(SessionState.Loading, first.sessions.value)
            assertNull(first.current())
            first.save(session)
            assertEquals(SessionState.SignedIn(session), first.sessions.value)
            closeAll()

            assertEquals(session, store().current())
        }

    @Test
    fun `the token is not on disk as it is`() =
        runTest {
            store().save(session)
            closeAll()
            val bytes = File(dir, "session.preferences_pb").readBytes()
            assertFalse(bytes.toByteString().indexOf("org_secret".encodeUtf8()) >= 0)
        }

    @Test
    fun `a token that no longer decrypts is signed out`() =
        runTest {
            store().save(session)
            closeAll()
            cipher.broken = true
            assertEquals(SessionState.SignedOut, store().load())
        }

    @Test
    fun `signing out keeps the server for next time`() =
        runTest {
            val store = store()
            store.save(session)
            store.clear()
            assertEquals(SessionState.SignedOut, store.sessions.first())
            assertNull(store.current())
            assertEquals("https://example.com", store.lastServer())
        }

    @Test
    fun `a pending sign-in is taken once`() =
        runTest {
            val store = store()
            store.savePending(PendingSignIn("https://example.com", "verifier", "Pixel"))
            assertEquals(PendingSignIn("https://example.com", "verifier", "Pixel"), store.takePending())
            assertNull(store.takePending())
        }

    @Test
    fun `the private key is kept encrypted and goes with the session`() =
        runTest {
            val key = ByteArray(32) { it.toByte() }
            val store = store()
            store.save(session)
            store.savePrivateKey(key)
            closeAll()
            val bytes = File(dir, "session.preferences_pb").readBytes()
            assertFalse(bytes.toByteString().indexOf(okio.ByteString.of(*key)) >= 0)

            val again = store()
            assertArrayEquals(key, again.loadPrivateKey())
            again.clear()
            assertNull(again.loadPrivateKey())
        }

    private fun ByteArray.toByteString() = okio.ByteString.of(*this)
}
