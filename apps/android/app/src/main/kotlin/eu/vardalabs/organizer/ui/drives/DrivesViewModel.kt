package eu.vardalabs.organizer.ui.drives

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.api.models.SharedItem
import eu.vardalabs.organizer.data.DrivesRepository
import eu.vardalabs.organizer.data.isMirror
import eu.vardalabs.organizer.data.isRemote
import eu.vardalabs.organizer.ui.files.OpenFile
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.File
import java.io.IOException
import java.util.UUID

data class DrivesState(
    /** My drive first, then the shared drives, the mirrors, the drives of other servers (as the server lists them). */
    val drives: List<Drive> = emptyList(),
    /** What is shared with the user. */
    val shared: List<SharedItem> = emptyList(),
    val loading: Boolean = true,
    val refreshing: Boolean = false,
    val error: String? = null,
    /** A short message for a snackbar, once. */
    val message: String? = null,
    /** What is being accepted or declined (a shared item's id, a remote drive's root): its buttons wait. */
    val answering: UUID? = null,
    /** The shared file being downloaded to open. */
    val opening: UUID? = null,
    /** A file to open in another app, once. */
    val toOpen: OpenFile? = null,
) {
    /** My drive and the shared drives: the list's first group. */
    val ownAndShared: List<Drive> get() = drives.filterNot { it.isMirror || it.isRemote }

    /** The mirrors (docs/M7D_DESIGN.md §5): a group of their own after the shared drives, read-only. */
    val mirrors: List<Drive> get() = drives.filter { it.isMirror }

    /** The drives shared from other servers (docs/M7E_DESIGN.md §6): "From other servers", last. */
    val remotes: List<Drive> get() = drives.filter { it.isRemote }
}

/**
 * Where the Files tab starts: the user's drives (My drive and the shared
 * drives they are a member of), each opening its root folder, and what is
 * shared with them. Read again when the live events say something changed
 * (a drive joined or left shows up so).
 */
class DrivesViewModel(
    private val repository: DrivesRepository,
    changes: Flow<Unit>,
    private val download: suspend (Node) -> File,
) : ViewModel() {
    private val state = MutableStateFlow(DrivesState())
    val ui: StateFlow<DrivesState> = state.asStateFlow()

    init {
        load()
        viewModelScope.launch { changes.collect { load() } }
    }

    fun refresh() {
        state.update { it.copy(refreshing = true) }
        load()
    }

    private fun load() {
        viewModelScope.launch {
            try {
                val drives = repository.drives()
                val shared = repository.sharedWithMe()
                state.update {
                    it.copy(drives = drives, shared = shared, loading = false, refreshing = false, error = null)
                }
            } catch (e: IOException) {
                state.update {
                    val shown = it.drives.isNotEmpty()
                    it.copy(
                        loading = false,
                        refreshing = false,
                        error = if (shown) it.error else e.message ?: "Cannot load the drives",
                        message = if (shown) e.message else null,
                    )
                }
            }
        }
    }

    /** A shared file: downloaded, then opened in another app. */
    fun open(node: Node) {
        if (state.value.opening != null) return
        state.update { it.copy(opening = node.id) }
        viewModelScope.launch {
            try {
                val file = download(node)
                state.update { it.copy(opening = null, toOpen = OpenFile(file, node.mime)) }
            } catch (e: IOException) {
                state.update { it.copy(opening = null, message = e.message ?: "Cannot download ${node.name}") }
            }
        }
    }

    /**
     * Accept what another server shared ([id]: the shared item's, or a
     * remote drive's root): its files are listed from then on.
     */
    fun accept(id: UUID) = answer(id, "Cannot accept it") { repository.accept(it) }

    /** Decline something shared from another server: it goes, and its server is told. */
    fun decline(id: UUID) = answer(id, "Cannot decline it") { repository.decline(it) }

    private fun answer(
        id: UUID,
        failed: String,
        call: suspend (UUID) -> Unit,
    ) {
        if (state.value.answering != null) return
        state.update { it.copy(answering = id) }
        viewModelScope.launch {
            try {
                call(id)
                state.update { it.copy(answering = null) }
                load()
            } catch (e: IOException) {
                state.update { it.copy(answering = null, message = e.message ?: failed) }
            }
        }
    }

    fun opened() = state.update { it.copy(toOpen = null) }

    fun messageShown() = state.update { it.copy(message = null) }
}

/** What a shared drive shows under its name: `3 members · 1.2 GB`. */
fun membersAndSize(
    members: Long,
    size: String,
): String = "$members ${if (members == 1L) "member" else "members"} · $size"
