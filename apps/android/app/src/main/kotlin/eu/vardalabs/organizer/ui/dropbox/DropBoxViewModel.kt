package eu.vardalabs.organizer.ui.dropbox

import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import eu.vardalabs.organizer.api.models.ItemView
import eu.vardalabs.organizer.api.models.Status
import eu.vardalabs.organizer.data.ANALYSING
import eu.vardalabs.organizer.data.DONE
import eu.vardalabs.organizer.data.DropBoxRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.UNPLACED
import eu.vardalabs.organizer.upload.UploadInfo
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException
import java.util.UUID

/** The drop box's items in the screen's sections, as the web groups them. */
data class Sections(
    /** Proposed: Confirm, Elsewhere, Reject. */
    val toReview: List<ItemView> = emptyList(),
    /** To place by hand or retry. */
    val needsYou: List<ItemView> = emptyList(),
    val analysing: List<ItemView> = emptyList(),
    /** Placed, with Undo. */
    val placed: List<ItemView> = emptyList(),
) {
    companion object {
        fun of(items: List<ItemView>) =
            Sections(
                toReview = items.filter { it.status == Status.PROPOSED },
                needsYou = items.filter { it.status in UNPLACED },
                analysing = items.filter { it.status in ANALYSING },
                placed = items.filter { it.status in DONE },
            )
    }
}

data class DropBoxState(
    val sections: Sections = Sections(),
    val loading: Boolean = true,
    val refreshing: Boolean = false,
    val error: String? = null,
    val message: String? = null,
    /** The item an action is running on. */
    val busy: UUID? = null,
    /** Files on their way to the drop box. */
    val uploads: List<UploadInfo> = emptyList(),
)

/** Screen 3: the drop box's review queue. */
class DropBoxViewModel(
    private val dropBox: DropBoxRepository,
    uploads: Flow<List<UploadInfo>>,
    private val enqueue: suspend (List<Uri>) -> Unit,
) : ViewModel() {
    private val state = MutableStateFlow(DropBoxState())
    val ui: StateFlow<DropBoxState> = state.asStateFlow()
    private val dismissed = mutableSetOf<UUID>()

    init {
        viewModelScope.launch {
            dropBox.items.collect { items -> state.update { it.copy(sections = Sections.of(items)) } }
        }
        viewModelScope.launch {
            uploads.collect { all ->
                state.update { s ->
                    s.copy(
                        uploads =
                            all.filter {
                                it.folderId == null && it.state != UploadInfo.State.DONE && it.id !in dismissed
                            },
                    )
                }
            }
        }
        load()
    }

    fun refresh() {
        state.update { it.copy(refreshing = true) }
        load()
    }

    private fun load() {
        viewModelScope.launch {
            try {
                dropBox.refresh()
                state.update { it.copy(loading = false, refreshing = false, error = null) }
            } catch (e: IOException) {
                state.update {
                    it.copy(loading = false, refreshing = false, error = e.message ?: "Cannot load the drop box")
                }
            }
        }
    }

    private fun act(
        item: ItemView,
        done: String?,
        action: suspend () -> Unit,
    ) {
        if (state.value.busy != null) return
        state.update { it.copy(busy = item.id) }
        viewModelScope.launch {
            try {
                action()
                state.update { it.copy(busy = null, message = done) }
            } catch (e: IOException) {
                state.update { it.copy(busy = null, message = e.message ?: "That did not work") }
                runCatching { dropBox.refresh() }
            }
        }
    }

    fun confirm(item: ItemView) = act(item, "Confirmed") { dropBox.confirm(item) }

    fun elsewhere(
        item: ItemView,
        folder: Folder,
    ) = act(item, "Placed in ${folder.location}") {
        dropBox.confirmElsewhere(
            item,
            folder.id.takeIf {
                folder.parentId !=
                    null
            },
        )
    }

    fun reject(item: ItemView) = act(item, "Rejected: place it by hand") { dropBox.reject(item) }

    fun place(
        item: ItemView,
        folder: Folder,
    ) = act(item, "Placed in ${folder.location}") { dropBox.place(item, folder.id.takeIf { folder.parentId != null }) }

    fun retry(item: ItemView) = act(item, "Analysing again") { dropBox.retry(item) }

    fun undo(item: ItemView) = act(item, "Undone: it is back in the drop box") { dropBox.undo(item) }

    fun addFiles(uris: List<Uri>) {
        if (uris.isEmpty()) return
        viewModelScope.launch {
            try {
                enqueue(uris)
            } catch (e: IOException) {
                state.update { it.copy(message = e.message ?: "Cannot add the files") }
            } catch (e: SecurityException) {
                state.update { it.copy(message = "Organizer may not read that file") }
            }
        }
    }

    fun addLink(
        url: String,
        note: String?,
    ) {
        viewModelScope.launch {
            try {
                dropBox.addLink(url, note)
                state.update { it.copy(message = "Added") }
            } catch (e: IOException) {
                state.update { it.copy(message = e.message ?: "Cannot add the link") }
            }
        }
    }

    fun dismissUpload(id: UUID) {
        dismissed += id
        state.update { s -> s.copy(uploads = s.uploads.filter { it.id != id }) }
    }

    fun messageShown() = state.update { it.copy(message = null) }
}
