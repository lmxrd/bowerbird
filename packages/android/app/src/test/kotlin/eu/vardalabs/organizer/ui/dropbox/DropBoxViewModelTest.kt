package eu.vardalabs.organizer.ui.dropbox

import eu.vardalabs.organizer.MainDispatcherRule
import eu.vardalabs.organizer.ROOT
import eu.vardalabs.organizer.api.models.DropBoxSettings
import eu.vardalabs.organizer.api.models.ItemView
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.api.models.Status
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.data.DropBoxRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.WAITING
import eu.vardalabs.organizer.item
import eu.vardalabs.organizer.upload.UploadInfo
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.util.UUID

class FakeDropBox(
    initial: List<ItemView>,
) : DropBoxRepository {
    override val items = MutableStateFlow(initial)
    val calls = mutableListOf<String>()
    var fail: ApiException? = null

    override suspend fun refresh() {
        calls += "refresh"
    }

    private fun set(
        item: ItemView,
        status: Status,
        call: String,
    ) {
        fail?.let { throw it }
        calls += call
        items.value = items.value.map { if (it.id == item.id) it.copy(status = status) else it }
    }

    override suspend fun confirm(item: ItemView) = set(item, Status.CONFIRMED, "confirm ${item.name}")

    override suspend fun confirmElsewhere(
        item: ItemView,
        folderId: UUID?,
    ) = set(item, Status.CHANGED, "elsewhere ${item.name} $folderId")

    override suspend fun reject(item: ItemView) = set(item, Status.REJECTED, "reject ${item.name}")

    override suspend fun place(
        item: ItemView,
        folderId: UUID?,
    ) = set(item, Status.CONFIRMED, "place ${item.name} $folderId")

    override suspend fun retry(item: ItemView) = set(item, Status.QUEUED, "retry ${item.name}")

    override suspend fun undo(item: ItemView) = set(item, Status.UNDONE, "undo ${item.name}")

    override suspend fun addLink(
        url: String,
        note: String?,
    ) {
        calls += "link $url $note"
    }

    override suspend fun settings(): DropBoxSettings = eu.vardalabs.organizer.settings()
}

class DropBoxViewModelTest {
    @get:Rule val main = MainDispatcherRule()

    private val proposed = item("receipt.pdf", Status.PROPOSED)
    private val failed = item("scan.pdf", Status.FAILED)
    private val analysing = item("photo.jpg", Status.ANALYSING)
    private val placed = item("invoice.pdf", Status.AUTO_ACCEPTED, placed = true)
    private val dropBox = FakeDropBox(listOf(proposed, failed, analysing, placed))
    private val uploads = MutableStateFlow<List<UploadInfo>>(emptyList())

    private fun viewModel() = DropBoxViewModel(dropBox, uploads) {}

    @Test
    fun `items are in the web's sections`() {
        val sections = viewModel().ui.value.sections
        assertEquals(listOf(proposed), sections.toReview)
        assertEquals(listOf(failed), sections.needsYou)
        assertEquals(listOf(analysing), sections.analysing)
        assertEquals(listOf(placed), sections.placed)
    }

    @Test
    fun `the badge counts what waits for the user`() {
        assertEquals(2, dropBox.items.value.count { it.status in WAITING })
    }

    @Test
    fun `review actions call the server and move the item`() {
        val vm = viewModel()
        vm.confirm(proposed)
        assertEquals("Confirmed", vm.ui.value.message)
        assertTrue(
            vm.ui.value.sections.placed
                .any { it.id == proposed.id },
        )
        vm.retry(failed)
        vm.undo(placed)
        assertEquals(listOf("refresh", "confirm receipt.pdf", "retry scan.pdf", "undo invoice.pdf"), dropBox.calls)
        assertNull(vm.ui.value.busy)
    }

    @Test
    fun `elsewhere and place send the folder, or null for the top`() {
        val vm = viewModel()
        val taxes = Folder(UUID.randomUUID(), "Taxes", "/Taxes", ROOT.id, role = Role.OWNER)
        vm.elsewhere(proposed, taxes)
        vm.place(failed, ROOT)
        assertEquals(listOf("elsewhere receipt.pdf ${taxes.id}", "place scan.pdf null"), dropBox.calls.drop(1))
    }

    @Test
    fun `a refused action is said`() {
        val vm = viewModel()
        dropBox.fail = ApiException(409, "The item is not proposed")
        vm.reject(proposed)
        assertEquals("The item is not proposed", vm.ui.value.message)
        assertNull(vm.ui.value.busy)
    }

    @Test
    fun `only uploads into the drop box show`() {
        val vm = viewModel()
        val mine = UploadInfo(UUID.randomUUID(), "a", null, UploadInfo.State.RUNNING, 1, 2, null)
        val folder = mine.copy(id = UUID.randomUUID(), folderId = ROOT.id.toString())
        uploads.value = listOf(mine, folder)
        assertEquals(listOf(mine), vm.ui.value.uploads)
    }
}
