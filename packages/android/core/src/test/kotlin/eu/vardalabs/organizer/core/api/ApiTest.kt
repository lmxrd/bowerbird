package eu.vardalabs.organizer.core.api

import eu.vardalabs.organizer.core.session.Session
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.Assert.assertEquals
import org.junit.Test

class ApiTest {
    private fun session(web: MockWebServer) = Session(web.url("/").toString().trimEnd('/'), "org_t", "org_t", "Pixel")

    @Test
    fun `a device signs out with its own token`() {
        MockWebServer().use { web ->
            web.enqueue(MockResponse.Builder().code(204).build())
            web.start()
            Api(session(web), OkHttpClient()) {}.auth.signOutDevice()
            val request = web.takeRequest()
            assertEquals("DELETE", request.method)
            assertEquals("/api/auth/device", request.url.encodedPath)
            assertEquals("Bearer org_t", request.headers["Authorization"])
        }
    }

    @Test
    fun `a 401 signs the app out`() {
        MockWebServer().use { web ->
            web.enqueue(
                MockResponse
                    .Builder()
                    .code(401)
                    .body("""{"error":"Not signed in"}""")
                    .build(),
            )
            web.start()
            var signedOut = false
            try {
                Api(session(web), OkHttpClient()) { signedOut = true }.auth.getSession()
            } catch (_: Exception) {
            }
            assertEquals(true, signedOut)
        }
    }
}
