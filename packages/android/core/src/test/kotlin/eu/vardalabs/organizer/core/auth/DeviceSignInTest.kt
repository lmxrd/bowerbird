package eu.vardalabs.organizer.core.auth

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.session.FakeCipher
import eu.vardalabs.organizer.core.session.Session
import eu.vardalabs.organizer.core.session.SessionStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.HttpUrl
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.File
import java.io.IOException
import java.nio.file.Files

class DeviceSignInTest {
    private val dir: File = Files.createTempDirectory("signin").toFile()
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val store =
        SessionStore(PreferenceDataStoreFactory.create(scope = scope) { File(dir, "s.preferences_pb") }, FakeCipher())

    @After
    fun cleanUp() {
        scope.cancel()
        dir.deleteRecursively()
    }

    private class FakeServer(
        val reachable: Boolean = true,
    ) : DeviceLoginServer {
        val exchanged = mutableListOf<Triple<HttpUrl, String, String>>()

        override suspend fun check(server: HttpUrl) {
            if (!reachable) throw IOException("unreachable")
        }

        override suspend fun exchange(
            server: HttpUrl,
            code: String,
            verifier: String,
        ): IssuedToken {
            exchanged += Triple(server, code, verifier)
            return IssuedToken("org_token", "org_abcd")
        }
    }

    @Test
    fun `a sign-in opens the authorize page, then exchanges the redirect's code`() =
        runTest {
            val server = FakeServer()
            val signIn = DeviceSignIn(store, server) { "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk" }
            val url = signIn.start("files.example.com", "Pixel 9")
            assertEquals("files.example.com", url.host)
            assertEquals("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", url.queryParameter("code_challenge"))

            val session = signIn.complete("organizer://auth?code=c0de")
            assertEquals(
                Session("https://files.example.com", "org_token", "org_abcd", "Pixel 9"),
                session,
            )
            assertEquals("c0de", server.exchanged.single().second)
            assertEquals("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", server.exchanged.single().third)
            assertEquals(session, store.current())
        }

    @Test
    fun `a redirect without a sign-in started is refused`() =
        runTest {
            try {
                DeviceSignIn(store, FakeServer()).complete("organizer://auth?code=c0de")
                fail()
            } catch (e: ApiException) {
                assertEquals(400, e.status)
            }
            assertNull(store.current())
        }

    @Test
    fun `an unreachable server is said before the browser opens`() =
        runTest {
            try {
                DeviceSignIn(store, FakeServer(reachable = false)).start("example.com", "Pixel")
                fail()
            } catch (_: IOException) {
            }
            assertNull(store.takePending())
        }

    @Test
    fun `the exchange posts the code and verifier and reads the token`() =
        runTest {
            MockWebServer().use { web ->
                web.enqueue(
                    MockResponse
                        .Builder()
                        .code(200)
                        .addHeader("Content-Type", "application/json")
                        .body(
                            """{"token":"org_t","key":{"id":"0190b3d4-0000-7000-8000-000000000001","kind":"device",""" +
                                """"name":"Pixel","prefix":"org_t","scope":"write","created_at":"t",""" +
                                """"last_used_at":null,"expires_at":null}}""",
                        ).build(),
                )
                web.start()
                val issued = HttpDeviceLoginServer(OkHttpClient()).exchange(web.url("/"), "c0de", "v3rifier")
                assertEquals(IssuedToken("org_t", "org_t"), issued)
                val request = web.takeRequest()
                assertEquals("/api/auth/device/token", request.url.encodedPath)
                val body = request.body!!.utf8()
                assertTrue(body.contains("\"code\":\"c0de\"") && body.contains("\"code_verifier\":\"v3rifier\""))
            }
        }
}
