package eu.vardalabs.organizer.push

import eu.vardalabs.organizer.api.models.Notice
import eu.vardalabs.organizer.api.models.NoticeKind
import eu.vardalabs.organizer.core.api.ApiException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.IOException
import java.util.UUID

class FakePushServer : PushServer {
    val calls = mutableListOf<String>()
    var notices = listOf<Notice>()
    var failure: IOException? = null
    var nextId = 1

    override suspend fun vapidKey(): String {
        failure?.let { throw it }
        calls += "vapid"
        return "BVapid"
    }

    override suspend fun subscribe(
        name: String,
        endpoint: String,
        p256dh: String,
        auth: String,
    ): String {
        failure?.let { throw it }
        calls += "subscribe unifiedpush $name $endpoint $p256dh $auth"
        return "sub-${nextId++}"
    }

    override suspend fun unsubscribe(id: String) {
        failure?.let { throw it }
        calls += "unsubscribe $id"
    }

    override suspend fun news(): List<Notice> {
        failure?.let { throw it }
        calls += "news"
        return notices
    }
}

class FakeDistributor(
    /** The distributor installed, or null. */
    var installed: String? = "ntfy",
) : Distributor {
    val calls = mutableListOf<String>()
    var registered = false

    override fun current(): String? = installed?.takeIf { registered }

    override fun resolve(): String? = installed

    override fun register(vapid: String) {
        calls += "register $vapid"
        registered = true
    }

    override fun unregister() {
        calls += "unregister"
        registered = false
    }
}

class MemoryPushPrefs : PushPrefs {
    override var enabled = true
    override var subscriptionId: String? = null
    override var lastNotice: String? = null
}

fun notice(
    id: String,
    path: String = "/drop-box",
) = Notice(
    body = "3 items to review",
    createdAt = "2026-11-08T10:00:00Z",
    id = UUID.nameUUIDFromBytes(id.toByteArray()),
    kind = NoticeKind.REVIEW,
    path = path,
    title = "Drop box",
)

class PushNotificationsTest {
    private val server = FakePushServer()
    private val distributor = FakeDistributor()
    private val prefs = MemoryPushPrefs()
    private var signedIn = true

    private val push = PushNotifications({ server.takeIf { signedIn } }, distributor, prefs, "Pixel 9")

    @Test
    fun `after sign-in it registers with the server's VAPID key`() =
        runTest {
            push.start()
            assertEquals(listOf("register BVapid"), distributor.calls)
            assertEquals(PushStatus(on = true, distributor = "ntfy"), push.status.value)
        }

    @Test
    fun `without a distributor nothing happens`() =
        runTest {
            distributor.installed = null
            push.start()
            assertEquals(emptyList<String>(), distributor.calls)
            assertEquals(emptyList<String>(), server.calls)
            assertFalse(push.status.value.on)
        }

    @Test
    fun `turned off on this device, it does not register`() =
        runTest {
            prefs.enabled = false
            push.start()
            assertEquals(emptyList<String>(), distributor.calls)
        }

    @Test
    fun `a new endpoint is sent as a unifiedpush subscription with its keys`() =
        runTest {
            push.onNewEndpoint("https://ntfy.sh/upAbc", "BKey", "authSecret")
            assertEquals(listOf("subscribe unifiedpush Pixel 9 https://ntfy.sh/upAbc BKey authSecret"), server.calls)
            assertEquals("sub-1", prefs.subscriptionId)
        }

    @Test
    fun `another endpoint replaces the old subscription`() =
        runTest {
            push.onNewEndpoint("https://ntfy.sh/a", "k", "a")
            push.onNewEndpoint("https://ntfy.sh/b", "k", "a")
            assertEquals("unsubscribe sub-1", server.calls.last())
            assertEquals("sub-2", prefs.subscriptionId)
        }

    @Test
    fun `an endpoint that cannot be sent fails, to be tried again`() =
        runTest {
            server.failure = IOException("offline")
            try {
                push.onNewEndpoint("https://ntfy.sh/a", "k", "a")
                fail("no error")
            } catch (_: IOException) {
            }
            assertNull(prefs.subscriptionId)
        }

    @Test
    fun `an unregistration removes the subscription`() =
        runTest {
            prefs.subscriptionId = "sub-7"
            push.onUnregistered()
            assertEquals(listOf("unsubscribe sub-7"), server.calls)
            assertNull(prefs.subscriptionId)
        }

    @Test
    fun `a subscription the server forgot already is no error`() =
        runTest {
            prefs.subscriptionId = "sub-7"
            val gone =
                object : PushServer by server {
                    override suspend fun unsubscribe(id: String) = throw ApiException(404, "Not found")
                }
            PushNotifications({ gone }, distributor, prefs, "Pixel 9").onUnregistered()
            assertNull(prefs.subscriptionId)
        }

    @Test
    fun `signing out unregisters and removes the subscription, before the token goes`() =
        runTest {
            push.start()
            prefs.subscriptionId = "sub-1"
            prefs.lastNotice = "n1"
            push.signOut()
            assertEquals(listOf("register BVapid", "unregister"), distributor.calls)
            assertEquals("unsubscribe sub-1", server.calls.last())
            assertNull(prefs.subscriptionId)
            assertNull(prefs.lastNotice)
            assertTrue(prefs.enabled)
        }

    @Test
    fun `signing out offline still stops here`() =
        runTest {
            prefs.subscriptionId = "sub-1"
            server.failure = IOException("offline")
            push.signOut()
            assertEquals(listOf("unregister"), distributor.calls)
            assertNull(prefs.subscriptionId)
        }

    @Test
    fun `the switch turns notifications off and on`() =
        runTest {
            push.start()
            prefs.subscriptionId = "sub-1"
            push.disable()
            assertFalse(prefs.enabled)
            assertFalse(push.status.value.on)
            assertEquals("unsubscribe sub-1", server.calls.last())
            push.enable(picked = true)
            assertTrue(prefs.enabled)
            assertEquals(PushStatus(on = true, distributor = "ntfy"), push.status.value)
        }

    @Test
    fun `turning on without a distributor says so`() =
        runTest {
            prefs.enabled = false
            push.enable(picked = false)
            assertEquals(PushStatus(on = false, missing = true), push.status.value)
            assertFalse(prefs.enabled)
        }

    @Test
    fun `a push shows the newest notice`() =
        runTest {
            server.notices = listOf(notice("n2"), notice("n1", "/files"))
            assertEquals(notice("n2").id, push.onMessage()?.id)
            assertEquals(listOf("news"), server.calls)
        }

    @Test
    fun `a push with no notices shows nothing`() =
        runTest {
            assertNull(push.onMessage())
        }

    @Test
    fun `a notice shown already is not shown again`() =
        runTest {
            server.notices = listOf(notice("n1"))
            assertEquals(notice("n1").id, push.onMessage()?.id)
            assertNull(push.onMessage())
            server.notices = listOf(notice("n2"), notice("n1"))
            assertEquals(notice("n2").id, push.onMessage()?.id)
        }

    @Test
    fun `signed out, a push fetches nothing`() =
        runTest {
            signedIn = false
            assertNull(push.onMessage())
            assertEquals(emptyList<String>(), server.calls)
        }

    @Test
    fun `news out of reach is an error, to be tried again`() =
        runTest {
            server.failure = ApiException(503, "Down")
            try {
                push.onMessage()
                fail("no error")
            } catch (e: ApiException) {
                assertTrue(e.retryable)
            }
        }
}
