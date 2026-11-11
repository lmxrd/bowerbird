package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Choice
import eu.vardalabs.organizer.api.models.DropBoxSettings
import eu.vardalabs.organizer.api.models.DropLink
import eu.vardalabs.organizer.api.models.ItemView
import eu.vardalabs.organizer.api.models.Status
import eu.vardalabs.organizer.core.api.Api
import eu.vardalabs.organizer.core.api.apiCall
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.util.UUID

/** The statuses of items that wait for the user (the web's `WAITING`): the badge. */
val WAITING =
    setOf(Status.PROPOSED, Status.NEEDS_MANUAL, Status.REJECTED, Status.FAILED, Status.WAITING, Status.UNDONE)

/** The statuses of items the AI is still on. */
val ANALYSING = setOf(Status.QUEUED, Status.ANALYSING)

/** The statuses of placed and accepted items. */
val DONE = setOf(Status.CONFIRMED, Status.CHANGED, Status.AUTO_ACCEPTED)

/** The statuses of items to place by hand, or to try again. */
val UNPLACED = setOf(Status.NEEDS_MANUAL, Status.REJECTED, Status.FAILED, Status.WAITING, Status.UNDONE)

/** The drop box's items, shared by its screen and the navigation's badge. */
interface DropBoxRepository {
    /** The newest first; empty until the first [refresh]. */
    val items: StateFlow<List<ItemView>>

    suspend fun refresh()

    /** Accept the proposal. */
    suspend fun confirm(item: ItemView)

    /** Put it elsewhere than proposed (kept as an example for the next proposals); null for the top. */
    suspend fun confirmElsewhere(
        item: ItemView,
        folderId: UUID?,
    )

    suspend fun reject(item: ItemView)

    /** Place an unplaced item by hand; null for the top. */
    suspend fun place(
        item: ItemView,
        folderId: UUID?,
    )

    suspend fun retry(item: ItemView)

    /** Undo the item's placement: it goes back to the drop box. */
    suspend fun undo(item: ItemView)

    suspend fun addLink(
        url: String,
        note: String?,
    )

    /** The user's drop-box settings: which per-item options to offer. */
    suspend fun settings(): DropBoxSettings
}

class ApiDropBoxRepository(
    private val api: suspend () -> Api?,
) : DropBoxRepository {
    private val state = MutableStateFlow<List<ItemView>>(emptyList())
    override val items: StateFlow<List<ItemView>> = state.asStateFlow()

    private suspend fun api() = api.invoke() ?: throw SignedOutException()

    override suspend fun refresh() {
        val api = api()
        state.value = apiCall { api.dropBox.listDropBoxItems(limit = 500) }
    }

    /** Run an action that answers the item, and put the answer in the list. */
    private suspend fun update(action: (Api) -> ItemView) {
        val api = api()
        val updated = apiCall { action(api) }
        state.value = state.value.map { if (it.id == updated.id) updated else it }
    }

    override suspend fun confirm(item: ItemView) = update { it.dropBox.confirmDropBoxItem(item.id) }

    override suspend fun confirmElsewhere(
        item: ItemView,
        folderId: UUID?,
    ) = update { it.dropBox.confirmDropBoxItem(item.id, Choice(folderId = folderId)) }

    override suspend fun reject(item: ItemView) = update { it.dropBox.rejectDropBoxItem(item.id) }

    override suspend fun place(
        item: ItemView,
        folderId: UUID?,
    ) = update { it.dropBox.placeDropBoxItem(item.id, Choice(folderId = folderId)) }

    override suspend fun retry(item: ItemView) = update { it.dropBox.retryDropBoxItem(item.id) }

    override suspend fun undo(item: ItemView) {
        val placement = item.placement ?: return
        update { it.dropBox.undoPlacement(placement.id) }
    }

    override suspend fun addLink(
        url: String,
        note: String?,
    ) {
        val api = api()
        apiCall { api.dropBox.dropLink(DropLink(url = url.trim(), note = note?.trim()?.ifEmpty { null })) }
        refresh()
    }

    override suspend fun settings(): DropBoxSettings {
        val api = api()
        val view = apiCall { api.dropBox.getDropBoxSettings() }
        return DropBoxSettings(
            folderFreedom = view.folderFreedom,
            folders = view.folders,
            links = view.links,
            pendingMode = view.pendingMode,
            readContents = view.readContents,
        )
    }

    /** Signed out: nothing to show. */
    fun clear() {
        state.value = emptyList()
    }
}
