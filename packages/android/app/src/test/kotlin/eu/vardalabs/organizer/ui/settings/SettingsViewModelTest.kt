package eu.vardalabs.organizer.ui.settings

import eu.vardalabs.organizer.MainDispatcherRule
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.session.Session
import eu.vardalabs.organizer.push.Notifications
import eu.vardalabs.organizer.push.PushStatus
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.io.IOException

class SettingsViewModelTest {
    @get:Rule val main = MainDispatcherRule()

    private val session = Session("https://example.com", "org_t", "org_3f9a", "Pixel 9")
    private val events = mutableListOf<String>()

    private inner class FakeNotifications : Notifications {
        override val status = MutableStateFlow(PushStatus(on = true, distributor = "ntfy"))
        var failure: IOException? = null

        override suspend fun enable(picked: Boolean) {
            events += "enable $picked"
            status.value =
                if (picked) PushStatus(on = true, distributor = "ntfy") else PushStatus(on = false, missing = true)
        }

        override suspend fun disable() {
            failure?.let { throw it }
            events += "disable"
            status.value = PushStatus(on = false)
        }

        override suspend fun signOut() {
            failure?.let { throw it }
            events += "stop notifications"
        }
    }

    private val notifications = FakeNotifications()

    private fun viewModel(
        email: suspend () -> String? = { "anna@example.com" },
        signOutDevice: suspend () -> Unit = { events += "sign out device" },
    ) = SettingsViewModel(session, email, notifications, signOutDevice, forget = { events += "forget" })

    @Test
    fun `it shows the server, who is signed in and this device`() {
        val state = viewModel().ui.value
        assertEquals("https://example.com", state.server)
        assertEquals("anna@example.com", state.email)
        assertEquals("Pixel 9", state.deviceName)
        assertEquals("org_3f9a", state.keyPrefix)
        assertEquals(PushStatus(on = true, distributor = "ntfy"), state.notifications)
    }

    @Test
    fun `an email out of reach is left blank`() {
        assertNull(viewModel(email = { throw IOException("offline") }).ui.value.email)
    }

    @Test
    fun `signing out stops the notifications, revokes the device's token, then forgets it`() {
        viewModel().signOut()
        assertEquals(listOf("stop notifications", "sign out device", "forget"), events)
    }

    @Test
    fun `a token revoked already, or no network, still signs out here`() {
        viewModel(signOutDevice = { throw ApiException(401, "Not signed in") }).signOut()
        notifications.failure = IOException("offline")
        viewModel(signOutDevice = { throw IOException("offline") }).signOut()
        assertEquals(listOf("stop notifications", "forget", "forget"), events)
    }

    @Test
    fun `the notifications switch turns off and on`() {
        val viewModel = viewModel()
        viewModel.disableNotifications()
        assertFalse(viewModel.ui.value.notifications.on)
        viewModel.enableNotifications(picked = true)
        assertTrue(viewModel.ui.value.notifications.on)
        assertEquals(listOf("disable", "enable true"), events)
    }

    @Test
    fun `turning on with no distributor says so`() {
        val viewModel = viewModel()
        viewModel.enableNotifications(picked = false)
        assertTrue(viewModel.ui.value.notifications.missing)
    }

    @Test
    fun `the switch offline is no crash, and can be tried again`() {
        val viewModel = viewModel()
        notifications.failure = IOException("offline")
        viewModel.disableNotifications()
        assertFalse(viewModel.ui.value.switching)
        assertTrue(viewModel.ui.value.notifications.on)
    }
}
