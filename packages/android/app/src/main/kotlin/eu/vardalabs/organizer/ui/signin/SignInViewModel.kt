package eu.vardalabs.organizer.ui.signin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException

data class SignInState(
    val address: String = "",
    val busy: Boolean = false,
    val error: String? = null,
    /** The page to open in a Custom Tab, once. */
    val openUrl: String? = null,
)

/**
 * Screen 1: the server's address, then the device login in a Custom Tab.
 * [start] answers the page to open, [complete] takes the redirect back.
 */
class SignInViewModel(
    private val deviceName: String,
    private val start: suspend (address: String, deviceName: String) -> String,
    private val complete: suspend (redirect: String) -> Unit,
    lastServer: suspend () -> String?,
) : ViewModel() {
    private val state = MutableStateFlow(SignInState())
    val ui: StateFlow<SignInState> = state.asStateFlow()

    init {
        viewModelScope.launch {
            val last = lastServer()
            if (last != null) state.update { if (it.address.isEmpty()) it.copy(address = last) else it }
        }
    }

    fun onAddress(address: String) = state.update { it.copy(address = address, error = null) }

    fun onContinue() {
        val address = state.value.address
        if (address.isBlank() || state.value.busy) return
        state.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                val url = start(address, deviceName)
                state.update { it.copy(busy = false, openUrl = url) }
            } catch (e: IllegalArgumentException) {
                state.update { it.copy(busy = false, error = "That is not a server address") }
            } catch (e: IOException) {
                state.update { it.copy(busy = false, error = "Cannot reach the server: ${e.message}") }
            }
        }
    }

    /** The Custom Tab was opened (or could not be). */
    fun onOpened(error: String? = null) = state.update { it.copy(openUrl = null, error = error) }

    /** The browser sent us back to `organizer://auth`. */
    fun onRedirect(redirect: String) {
        state.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                complete(redirect)
                state.update { it.copy(busy = false) }
            } catch (e: IOException) {
                state.update { it.copy(busy = false, error = "The sign-in failed: ${e.message}") }
            }
        }
    }
}
