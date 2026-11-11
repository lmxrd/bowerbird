package eu.vardalabs.organizer.core.api

import eu.vardalabs.organizer.api.models.NoticeKind
import eu.vardalabs.organizer.core.session.Session
import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.Test

/** `GET /api/notifications/news` through the generated client: what a push stands for. */
class NewsTest {
    private fun api(web: MockWebServer) =
        Api(Session(web.url("/").toString().trimEnd('/'), "org_t", "org_t", "Pixel"), OkHttpClient()) {}

    @Test
    fun `the news is read with the device's token, newest first`() =
        runTest {
            MockWebServer().use { web ->
                web.enqueue(
                    MockResponse
                        .Builder()
                        .addHeader("Content-Type", "application/json")
                        .body(
                            """
                            {"review": 3, "notices": [
                              {"id": "0b7d6c1e-2f0a-4c55-9d43-0d3c8e1f0a01", "kind": "review", "title": "Drop box",
                               "body": "3 items to review", "path": "/drop-box", "created_at": "2026-11-08T10:00:00Z"},
                              {"id": "0b7d6c1e-2f0a-4c55-9d43-0d3c8e1f0a02", "kind": "shared", "title": "Shared",
                               "body": "Kari shared Recipes", "path": "/files", "created_at": "2026-11-08T09:00:00Z"}
                            ]}
                            """.trimIndent(),
                        ).build(),
                )
                web.start()
                val api = api(web)
                val notices = apiCall { api.notifications.getNews() }.notices
                assertEquals(listOf(NoticeKind.REVIEW, NoticeKind.SHARED), notices.map { it.kind })
                assertEquals("3 items to review", notices.first().body)
                assertEquals("/drop-box", notices.first().path)
                val request = web.takeRequest()
                assertEquals("/api/notifications/news", request.url.encodedPath)
                assertEquals("Bearer org_t", request.headers["Authorization"])
            }
        }

    @Test
    fun `an error answer is an ApiException`() =
        runTest {
            MockWebServer().use { web ->
                web.enqueue(
                    MockResponse
                        .Builder()
                        .code(503)
                        .addHeader("Content-Type", "application/json")
                        .body("""{"error":"Down"}""")
                        .build(),
                )
                web.start()
                val api = api(web)
                try {
                    apiCall { api.notifications.getNews() }
                    fail("no error")
                } catch (e: ApiException) {
                    assertEquals(503, e.status)
                    assertEquals("Down", e.message)
                }
            }
        }
}
