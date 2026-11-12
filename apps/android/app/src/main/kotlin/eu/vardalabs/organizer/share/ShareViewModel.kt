package eu.vardalabs.organizer.share

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import eu.vardalabs.organizer.api.models.DropBoxSettings
import eu.vardalabs.organizer.api.models.Links
import eu.vardalabs.organizer.api.models.ReadContents
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.mayAdd
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException

enum class ShareMode { DROP_BOX, FOLDER }

/** What the user chose, for [ShareViewModel]'s `send`. */
data class ShareChoice(
    val mode: ShareMode,
    val note: String?,
    /** Null for the user's setting. */
    val readContents: Boolean?,
    /** For a link: null for the user's setting. */
    val linkMode: Links?,
    val folder: Folder?,
)

data class ShareState(
    val mode: ShareMode = ShareMode.DROP_BOX,
    val note: String = "",
    /** Offered only where the settings leave it to each item. */
    val offerReadContents: Boolean = false,
    val readContents: Boolean = false,
    /** Offered for a link. */
    val offerLinkMode: Boolean = false,
    val linkMode: Links? = null,
    val folder: Folder? = null,
    val sending: Boolean = false,
    val error: String? = null,
    val done: Boolean = false,
) {
    /** A folder must be one the user may add to (the picker offers only those). */
    val canSend: Boolean get() = !sending && (mode == ShareMode.DROP_BOX || folder?.role.mayAdd == true)
}

/**
 * Screen 4, the share target: "Drop box" (a note, and the per-item options
 * the user's drop-box settings allow) or "Place manually" (a folder), then
 * Send, which hands the items to the uploads and lets the activity finish.
 */
class ShareViewModel(
    private val hasLink: Boolean,
    settings: suspend () -> DropBoxSettings?,
    /** Whether this device holds the keys of the encrypted folders. */
    private val unlocked: suspend () -> Boolean,
    private val send: suspend (ShareChoice) -> Unit,
) : ViewModel() {
    private val state = MutableStateFlow(ShareState(offerLinkMode = hasLink))
    val ui: StateFlow<ShareState> = state.asStateFlow()

    /** The settings that decide the options; null until loaded or when they cannot be. */
    private var loaded: DropBoxSettings? = null

    init {
        viewModelScope.launch {
            // Offline or an old server: no options, the user's settings apply.
            val got = runCatching { settings() }.getOrNull() ?: return@launch
            loaded = got
            state.update {
                it.copy(
                    offerReadContents = got.readContents != ReadContents.ALWAYS,
                    readContents = got.readContents == ReadContents.DEFAULT_ON,
                    linkMode = if (hasLink) got.links else null,
                )
            }
        }
    }

    fun onMode(mode: ShareMode) = state.update { it.copy(mode = mode, error = null) }

    fun onNote(note: String) = state.update { it.copy(note = note.take(MAX_NOTE)) }

    fun onReadContents(read: Boolean) = state.update { it.copy(readContents = read) }

    fun onLinkMode(mode: Links) = state.update { it.copy(linkMode = mode) }

    fun onFolder(folder: Folder) {
        state.update { it.copy(folder = folder, error = null) }
        if (folder.e2eRoot != null) {
            viewModelScope.launch {
                if (!unlocked()) state.update { it.copy(error = LOCKED) }
            }
        }
    }

    fun onSend() {
        val current = state.value
        if (!current.canSend) return
        state.update { it.copy(sending = true, error = null) }
        val choice =
            ShareChoice(
                mode = current.mode,
                note = current.note.trim().ifEmpty { null },
                readContents = if (current.offerReadContents) current.readContents else null,
                // Only when it differs from the user's setting: else the setting applies.
                linkMode = current.linkMode?.takeIf { it != loaded?.links },
                folder = current.folder,
            )
        viewModelScope.launch {
            // Its names and contents are encrypted on this device, which needs the keys.
            if (choice.mode == ShareMode.FOLDER && choice.folder?.e2eRoot != null && !unlocked()) {
                state.update { it.copy(sending = false, error = LOCKED) }
                return@launch
            }
            try {
                send(choice)
                state.update { it.copy(sending = false, done = true) }
            } catch (e: IOException) {
                state.update { it.copy(sending = false, error = e.message ?: "Cannot send") }
            } catch (e: IllegalArgumentException) {
                state.update { it.copy(sending = false, error = e.message ?: "Cannot send") }
            } catch (e: SecurityException) {
                state.update { it.copy(sending = false, error = "Organizer may not read what was shared") }
            }
        }
    }

    companion object {
        /** The server's limit for a note. */
        const val MAX_NOTE = 2_000

        const val LOCKED = "This folder is encrypted. Unlock it in Organizer first."
    }
}
