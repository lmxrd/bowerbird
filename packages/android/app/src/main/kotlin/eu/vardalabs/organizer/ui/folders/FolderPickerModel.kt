package eu.vardalabs.organizer.ui.folders

import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.mayAdd
import eu.vardalabs.organizer.data.takesNew
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException
import java.util.UUID

data class FolderPickerState(
    /**
     * At the drive level: the drives to choose from (those whose folder is
     * there, whose storage is usable, which are not being moved, not mirrors
     * and not on another server); null inside a folder.
     */
    val drives: List<Drive>? = null,
    /** The folder on screen; null at the drive level. */
    val folder: Folder? = null,
    /** The folders in [folder]. */
    val children: List<Node> = emptyList(),
    val loading: Boolean = true,
    val error: String? = null,
    /** Whether there is a level above: a parent folder, or the drives above a drive's root. */
    val canGoUp: Boolean = false,
    /** Whether the folder on screen may be chosen. */
    val canPick: Boolean = false,
    /** The folder on screen is the user's to view only (`my_role` viewer): nothing may be put in it. */
    val viewOnly: Boolean = false,
)

/**
 * The folder picker's logic: with [drives], it starts at the drive level
 * (My drive and the shared drives, where a member may write) unless given
 * a [start] folder; a move starts at the folder it is in and stays in its
 * drive (the server refuses moves between drives). [exclude] is a folder
 * that cannot be chosen nor entered (the one being moved).
 *
 * Only a folder where the user may add (`my_role` upload only and up)
 * may be chosen: whatever picks, a move, an upload or the drop box's
 * placing, puts something in it. A view-only folder can still be entered,
 * as a folder below it may have been given a higher role.
 */
class FolderPickerModel(
    private val files: FilesRepository,
    private val drives: (suspend () -> List<Drive>)?,
    start: UUID?,
    private val exclude: UUID?,
    /** Whether a drive's root may be chosen (the drop box places items in folders only). */
    private val allowRoot: Boolean,
    private val scope: CoroutineScope,
) {
    private val state = MutableStateFlow(FolderPickerState())
    val ui: StateFlow<FolderPickerState> = state.asStateFlow()

    private var loading: Job? = null

    /**
     * Starting from the drives, the picker puts something new somewhere (an
     * upload, the drop box's placing): never on another server (a folder of
     * one, `Folder.remote`, nor a remote drive: `takesNew`). A move stays in
     * its drive, a remote one too, by the role.
     */
    private val local = drives != null

    init {
        if (start == null && drives != null) showDrives() else open(start)
    }

    private fun showDrives() {
        val drives = drives ?: return
        load {
            val list = drives().filter { it.takesNew }
            FolderPickerState(drives = list, loading = false)
        }
    }

    /** Enter a drive: its root folder. */
    fun openDrive(drive: Drive) {
        drive.rootId?.let(::open)
    }

    /** Enter a folder (the root of My drive for null). */
    fun open(id: UUID?) {
        if (id != null && id == exclude) return
        load {
            val folder = files.folder(id)
            val children =
                files.children(folder.id).filter {
                    it.kind == Kind.FOLDER && it.id != exclude && !(local && it.remote != null)
                }
            FolderPickerState(
                folder = folder,
                children = children,
                loading = false,
                canGoUp = folder.parentId != null || drives != null,
                canPick =
                    (allowRoot || folder.parentId != null) &&
                        folder.role.mayAdd &&
                        !folder.readOnly &&
                        !(local && folder.remote != null),
                viewOnly = !folder.role.mayAdd,
            )
        }
    }

    /** One level up: the parent folder, or from a drive's root the drives. */
    fun up() {
        val folder = state.value.folder ?: return
        val parent = folder.parentId
        when {
            parent != null -> open(parent)
            drives != null -> showDrives()
        }
    }

    private fun load(read: suspend () -> FolderPickerState) {
        loading?.cancel()
        // What is on screen stays (not choosable) until the next level is read.
        state.update { it.copy(loading = true, error = null, canPick = false) }
        loading =
            scope.launch {
                try {
                    state.value = read()
                } catch (e: IOException) {
                    state.update { it.copy(loading = false, error = e.message ?: "Cannot load the folder") }
                }
            }
    }
}
