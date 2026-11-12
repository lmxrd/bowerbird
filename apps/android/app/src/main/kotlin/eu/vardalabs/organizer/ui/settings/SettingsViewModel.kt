package eu.vardalabs.organizer.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import eu.vardalabs.organizer.core.session.Session
import eu.vardalabs.organizer.push.Notifications
import eu.vardalabs.organizer.push.PushStatus
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException

data class SettingsState(
    val server: String,
    val deviceName: String,
    val keyPrefix: String,
    /** Who the token belongs to; null until the server said. */
    val email: String? = null,
    val signingOut: Boolean = false,
    val notifications: PushStatus = PushStatus(on = false),
    /** The notifications switch waits for the server. */
    val switching: Boolean = false,
)

/**
 * Screen 5: the server, who is signed in, this device and its notifications.
 * Signing out stops the notifications (the subscription removed while the
 * token is still good), revokes the device's own token
 * (`DELETE /api/auth/device`), then forgets it here.
 */
class SettingsViewModel(
    session: Session,
    email: suspend () -> String?,
    private val notifications: Notifications,
    private val signOutDevice: suspend () -> Unit,
    private val forget: suspend () -> Unit,
) : ViewModel() {
    private val state = MutableStateFlow(SettingsState(session.server, session.deviceName, session.keyPrefix))
    val ui: StateFlow<SettingsState> = state.asStateFlow()

    init {
        viewModelScope.launch {
            // Offline: the line waits for the next visit.
            val got = runCatching { email() }.getOrNull()
            state.update { it.copy(email = got) }
        }
        viewModelScope.launch {
            notifications.status.collect { status -> state.update { it.copy(notifications = status) } }
        }
    }

    /** The switch turned on, once the screen let the user pick a distributor ([picked] false: there is none). */
    fun enableNotifications(picked: Boolean) = switch { notifications.enable(picked) }

    fun disableNotifications() = switch { notifications.disable() }

    private fun switch(action: suspend () -> Unit) {
        if (state.value.switching) return
        state.update { it.copy(switching = true) }
        viewModelScope.launch {
            try {
                action()
            } catch (_: IOException) {
                // Offline: the switch shows where things are; it can be tried again.
            }
            state.update { it.copy(switching = false) }
        }
    }

    fun signOut() {
        if (state.value.signingOut) return
        state.update { it.copy(signingOut = true) }
        viewModelScope.launch {
            try {
                notifications.signOut()
            } catch (_: IOException) {
                // Offline: the server's row goes once its push service says the endpoint is gone.
            }
            try {
                signOutDevice()
            } catch (_: IOException) {
                // Offline, or revoked already (401): forgotten here all the same.
            }
            forget()
        }
    }
}
