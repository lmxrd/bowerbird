package eu.vardalabs.organizer.ui.files

import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.core.e2e.LockedException
import eu.vardalabs.organizer.core.e2e.Unlocker
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.ItemActions
import eu.vardalabs.organizer.data.mayAdd
import eu.vardalabs.organizer.data.onlyOwnUploads
import eu.vardalabs.organizer.data.sealed
import eu.vardalabs.organizer.upload.UploadInfo
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.File
import java.io.IOException
import java.util.UUID

/** A file downloaded to open in another app. */
data class OpenFile(
    val file: File,
    val mime: String?,
)

data class FilesState(
    val folder: Folder? = null,
    val items: List<Node> = emptyList(),
    val loading: Boolean = true,
    val refreshing: Boolean = false,
    val error: String? = null,
    /** A short message for a snackbar, once. */
    val message: String? = null,
    /** The file being downloaded to open. */
    val opening: UUID? = null,
    /** A file to open in another app, once. */
    val toOpen: OpenFile? = null,
    /** Uploads into this folder that run, wait or failed. */
    val uploads: List<UploadInfo> = emptyList(),
    /** An encrypted folder, and this device has not unlocked the keys. */
    val locked: Boolean = false,
    val unlocking: Boolean = false,
    /** Why the passphrase did not unlock. */
    val unlockError: String? = null,
) {
    /**
     * Whether files and folders may be added here (upload, new folder):
     * upload only and up, and not while its drive is being moved.
     */
    val mayAdd: Boolean get() = folder?.role.mayAdd == true && !locked && folder?.readOnly != true

    /**
     * What may be done with an item of the folder: what its role allows (in
     * an encrypted folder a move seals the name again: on the web), and
     * nothing while its drive is being moved.
     */
    fun actions(node: Node): ItemActions =
        if (folder?.readOnly == true) {
            ItemActions(rename = false, move = false, delete = false)
        } else {
            ItemActions.of(node.myRole, sealed = node.sealed)
        }

    /** Upload only: the folder lists only what the user added. */
    val onlyOwnUploads: Boolean get() = folder?.role.onlyOwnUploads
}

/**
 * Screen 2, one folder: its files and folders, refreshed when the live
 * events say something changed, and what can be done with them. An
 * encrypted folder shows nothing until this device unlocks the keys
 * ([unlock], with the encryption passphrase); then its names are
 * decrypted by [files].
 */
class FilesViewModel(
    private val files: FilesRepository,
    private val folderId: UUID?,
    changes: Flow<Unit>,
    uploads: Flow<List<UploadInfo>>,
    private val enqueue: suspend (uris: List<Uri>, folder: Folder) -> Unit,
    private val encryption: Unlocker,
    private val dismissed: MutableSet<UUID> = mutableSetOf(),
) : ViewModel() {
    private val state = MutableStateFlow(FilesState())
    val ui: StateFlow<FilesState> = state.asStateFlow()

    /** Uploads seen running here, so that the folder is refreshed when one completes. */
    private val running = mutableSetOf<UUID>()
    private var latestUploads = emptyList<UploadInfo>()

    init {
        load(initial = true)
        viewModelScope.launch { changes.collect { load() } }
        viewModelScope.launch {
            uploads.collect {
                latestUploads = it
                applyUploads()
            }
        }
        // Unlocked on another screen: show what is inside.
        viewModelScope.launch {
            encryption.unlocked.collect { if (it && state.value.locked && !state.value.unlocking) load() }
        }
    }

    /** Show this folder's uploads; refresh when one that ran here completed. */
    private fun applyUploads() {
        val here =
            state.value.folder
                ?.id
                ?.toString() ?: return
        val mine = latestUploads.filter { it.folderId == here }
        val finished = mine.filter { it.state == UploadInfo.State.DONE && it.id in running }
        running += mine.filter { it.state != UploadInfo.State.DONE }.map { it.id }
        running -= finished.map { it.id }.toSet()
        state.update { s -> s.copy(uploads = mine.filter { it.state != UploadInfo.State.DONE && it.id !in dismissed }) }
        if (finished.isNotEmpty()) load()
    }

    fun refresh() {
        state.update { it.copy(refreshing = true) }
        load()
    }

    private fun load(initial: Boolean = false) {
        viewModelScope.launch {
            try {
                // Read again once unlocked: an encrypted folder's name opens only then.
                val folder = state.value.folder?.takeIf { !state.value.locked } ?: files.folder(folderId)
                val first = state.value.folder == null
                if (folder.e2eRoot != null && !encryption.isUnlocked()) {
                    showLocked(folder)
                } else {
                    val items = files.children(folder.id)
                    state.update {
                        it.copy(
                            folder = folder,
                            items = items,
                            loading = false,
                            refreshing = false,
                            error = null,
                            locked = false,
                        )
                    }
                }
                if (first) applyUploads()
            } catch (_: LockedException) {
                // Signed out meanwhile, or the keys forgotten.
                state.value.folder?.let(::showLocked)
            } catch (e: IOException) {
                state.update {
                    it.copy(
                        loading = false,
                        refreshing = false,
                        error = if (initial || it.folder == null) e.message ?: "Cannot load the folder" else it.error,
                        message = if (!initial && it.folder != null) e.message else null,
                    )
                }
            }
        }
    }

    private fun showLocked(folder: Folder) =
        state.update {
            it.copy(
                folder = folder,
                items = emptyList(),
                loading = false,
                refreshing = false,
                error = null,
                locked = true,
            )
        }

    /** Unlock this device's keys with the encryption passphrase, then show the folder. */
    fun unlock(passphrase: String) {
        if (passphrase.isEmpty() || state.value.unlocking) return
        state.update { it.copy(unlocking = true, unlockError = null) }
        viewModelScope.launch {
            try {
                encryption.unlock(passphrase)
                state.update { it.copy(unlocking = false) }
                load()
            } catch (e: IOException) {
                state.update { it.copy(unlocking = false, unlockError = e.message ?: "Cannot unlock") }
            }
        }
    }

    /** Run an action; on success refresh, on failure say why. */
    private fun act(
        done: String?,
        action: suspend () -> Unit,
    ) {
        viewModelScope.launch {
            try {
                action()
                state.update { it.copy(message = done) }
                load()
            } catch (e: IOException) {
                state.update { it.copy(message = e.message ?: "That did not work") }
            }
        }
    }

    /** Its drive is being moved: nothing is changed meanwhile (the server would refuse it). */
    private val readOnly: Boolean get() = state.value.folder?.readOnly == true

    fun createFolder(name: String) {
        val folder = state.value.folder ?: return
        if (name.isBlank() || !state.value.mayAdd) return
        act(null) { files.createFolder(folder, name) }
    }

    fun rename(
        node: Node,
        name: String,
    ) {
        if (name.isBlank() || name.trim() == node.name || !ItemActions.of(node.myRole).rename || readOnly) return
        act(null) { files.rename(node, name) }
    }

    fun move(
        node: Node,
        to: Folder,
    ) {
        if (to.id == node.parentId || !ItemActions.of(node.myRole).move || !to.role.mayAdd || readOnly) return
        act("Moved to ${to.location}") { files.move(node, to.id) }
    }

    fun delete(node: Node) {
        if (!ItemActions.of(node.myRole).delete || readOnly) return
        act("${node.name} is in the trash") { files.delete(node) }
    }

    fun open(node: Node) {
        if (state.value.opening != null) return
        state.update { it.copy(opening = node.id) }
        viewModelScope.launch {
            try {
                val file = files.download(node)
                state.update { it.copy(opening = null, toOpen = OpenFile(file, node.mime)) }
            } catch (e: IOException) {
                state.update { it.copy(opening = null, message = e.message ?: "Cannot download ${node.name}") }
            }
        }
    }

    fun upload(uris: List<Uri>) {
        val folder = state.value.folder ?: return
        if (uris.isEmpty() || !state.value.mayAdd) return
        viewModelScope.launch {
            try {
                enqueue(uris, folder)
            } catch (e: IOException) {
                state.update { it.copy(message = e.message ?: "Cannot upload") }
            } catch (e: SecurityException) {
                state.update { it.copy(message = "Organizer may not read that file") }
            }
        }
    }

    fun dismissUpload(id: UUID) {
        dismissed += id
        state.update { s -> s.copy(uploads = s.uploads.filter { it.id != id }) }
    }

    fun opened() = state.update { it.copy(toOpen = null) }

    fun messageShown() = state.update { it.copy(message = null) }
}
