package eu.vardalabs.organizer.ui.files

import eu.vardalabs.organizer.FakeUnlocker
import eu.vardalabs.organizer.MainDispatcherRule
import eu.vardalabs.organizer.ROOT
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.node
import eu.vardalabs.organizer.upload.UploadInfo
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.util.UUID

class FakeFiles : FilesRepository {
    val children = mutableMapOf<UUID, MutableList<Node>>(ROOT.id to mutableListOf())
    val calls = mutableListOf<String>()
    var loads = 0
    var fail: ApiException? = null
    val folders = mutableMapOf<UUID, Folder>()

    override suspend fun folder(id: UUID?): Folder = id?.let { folders[it] } ?: ROOT

    override suspend fun children(id: UUID): List<Node> {
        loads++
        fail?.let { throw it }
        return children[id].orEmpty().toList()
    }

    override suspend fun createFolder(
        parent: Folder,
        name: String,
    ): Node {
        calls += "create $name"
        return node(name, Kind.FOLDER).also { children.getValue(parent.id) += it }
    }

    override suspend fun rename(
        node: Node,
        name: String,
    ): Node {
        calls += "rename ${node.name} $name"
        return node.copy(name = name)
    }

    override suspend fun move(
        node: Node,
        to: UUID,
    ): Node {
        calls += "move ${node.name}"
        return node
    }

    override suspend fun delete(node: Node) {
        fail?.let { throw it }
        calls += "delete ${node.name}"
        children.getValue(ROOT.id).remove(node)
    }

    override suspend fun download(node: Node) = File("/cache/${node.name}")
}

class FilesViewModelTest {
    @get:Rule val main = MainDispatcherRule()

    private val files = FakeFiles()
    private val changes = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private val uploads = MutableStateFlow<List<UploadInfo>>(emptyList())

    private val encryption = FakeUnlocker()

    private fun viewModel(folderId: UUID? = null) =
        FilesViewModel(files, folderId, changes, uploads, { _, _ -> }, encryption)

    /** An encrypted folder (its own top) with a file in it, its name as the repository decrypts it. */
    private fun encryptedFolder(): Folder {
        val folder = Folder(UUID.randomUUID(), "Private", "/Private", ROOT.id, e2eRoot = null, role = Role.OWNER)
        val encrypted = folder.copy(e2eRoot = folder.id)
        files.folders[folder.id] = encrypted
        files.children[folder.id] = mutableListOf(node("taxes.pdf", parent = folder.id, e2eRoot = folder.id))
        return encrypted
    }

    @Test
    fun `an encrypted folder shows nothing until unlocked`() {
        val folder = encryptedFolder()
        val vm = viewModel(folder.id)
        val state = vm.ui.value
        assertTrue(state.locked)
        assertFalse(state.loading)
        assertEquals(folder, state.folder)
        assertEquals(emptyList<Node>(), state.items)
        assertEquals(0, files.loads)

        vm.unlock("wrong horse")
        assertTrue(vm.ui.value.locked)
        assertEquals("That passphrase does not unlock your keys", vm.ui.value.unlockError)

        vm.unlock(FakeUnlocker.PASSPHRASE)
        vm.ui.value.let {
            assertFalse(it.locked)
            assertFalse(it.unlocking)
            assertNull(it.unlockError)
            assertEquals(listOf("taxes.pdf"), it.items.map { node -> node.name })
        }
    }

    @Test
    fun `a locked folder opens when unlocked elsewhere`() {
        val folder = encryptedFolder()
        val vm = viewModel(folder.id)
        assertTrue(vm.ui.value.locked)
        encryption.unlocked.value = true
        assertFalse(vm.ui.value.locked)
        assertEquals(1, vm.ui.value.items.size)
    }

    @Test
    fun `an unlocked encrypted folder lists at once`() {
        val folder = encryptedFolder()
        encryption.unlocked.value = true
        val state = viewModel(folder.id).ui.value
        assertFalse(state.locked)
        assertEquals(listOf("taxes.pdf"), state.items.map { it.name })
    }

    @Test
    fun `it loads the root and its children`() {
        files.children.getValue(ROOT.id) += node("Taxes", Kind.FOLDER)
        val state = viewModel().ui.value
        assertEquals(ROOT, state.folder)
        assertEquals(listOf("Taxes"), state.items.map { it.name })
        assertFalse(state.loading)
    }

    @Test
    fun `a live event reloads the folder`() {
        val vm = viewModel()
        files.children.getValue(ROOT.id) += node("new.pdf")
        changes.tryEmit(Unit)
        assertEquals(
            listOf("new.pdf"),
            vm.ui.value.items
                .map { it.name },
        )
    }

    @Test
    fun `actions call the server, then reload`() {
        val file = node("a.pdf").also { files.children.getValue(ROOT.id) += it }
        val vm = viewModel()
        vm.createFolder("Receipts")
        vm.rename(file, "b.pdf")
        vm.rename(file, "a.pdf") // unchanged: nothing sent
        vm.delete(file)
        assertEquals(listOf("create Receipts", "rename a.pdf b.pdf", "delete a.pdf"), files.calls)
        assertEquals("a.pdf is in the trash", vm.ui.value.message)
        assertEquals(
            listOf("Receipts"),
            vm.ui.value.items
                .map { it.name },
        )
    }

    @Test
    fun `a viewer's folder offers nothing, and nothing is sent`() {
        val folder = Folder(UUID.randomUUID(), "Shared", "/Shared", ROOT.id, role = Role.VIEWER)
        val file = node("a.pdf", parent = folder.id, role = Role.VIEWER)
        files.folders[folder.id] = folder
        files.children[folder.id] = mutableListOf(file)
        val vm = viewModel(folder.id)
        assertFalse(vm.ui.value.mayAdd)
        assertFalse(vm.ui.value.onlyOwnUploads)
        vm.createFolder("Receipts")
        vm.rename(file, "b.pdf")
        vm.move(file, ROOT)
        vm.delete(file)
        assertEquals(emptyList<String>(), files.calls)
    }

    @Test
    fun `a drive being moved is read-only, whatever the role`() {
        val folder =
            Folder(UUID.randomUUID(), "Team", "/", null, driveName = "Team", role = Role.MANAGER, movingTo = "S3")
        val file = node("a.pdf", parent = folder.id, role = Role.MANAGER)
        files.folders[folder.id] = folder
        files.children[folder.id] = mutableListOf(file)
        val vm = viewModel(folder.id)
        assertTrue(
            vm.ui.value.folder!!
                .readOnly,
        )
        assertFalse(vm.ui.value.mayAdd)
        assertFalse(
            vm.ui.value
                .actions(file)
                .any,
        )
        vm.createFolder("Receipts")
        vm.rename(file, "b.pdf")
        vm.move(file, folder.copy(movingTo = null))
        vm.delete(file)
        assertEquals(emptyList<String>(), files.calls)
    }

    @Test
    fun `a mirror is browsed read-only`() {
        val folder =
            Folder(
                UUID.randomUUID(),
                "Team · mirror",
                "/",
                null,
                driveName = "Team · mirror",
                role = Role.VIEWER,
                mirror = true,
            )
        val file = node("a.pdf", parent = folder.id, role = Role.VIEWER, sourceId = UUID.randomUUID())
        files.folders[folder.id] = folder
        files.children[folder.id] = mutableListOf(file)
        val vm = viewModel(folder.id)
        assertEquals(
            listOf("a.pdf"),
            vm.ui.value.items
                .map { it.name },
        )
        assertFalse(vm.ui.value.mayAdd)
        assertFalse(
            vm.ui.value
                .actions(file)
                .any,
        )
        vm.createFolder("Receipts")
        vm.rename(file, "b.pdf")
        vm.delete(file)
        assertEquals(emptyList<String>(), files.calls)
    }

    @Test
    fun `a drive's storage unavailable says so in the server's words`() {
        val file = node("a.pdf").also { files.children.getValue(ROOT.id) += it }
        val vm = viewModel()
        files.fail =
            ApiException.of(
                503,
                """{"error":"Locked until alice@example.com signs in","code":"storage_unavailable"}""",
            )
        vm.delete(file)
        assertEquals("This drive is not available now: Locked until alice@example.com signs in.", vm.ui.value.message)
    }

    @Test
    fun `upload only adds and shows only one's own files, which it does not change`() {
        val folder = Folder(UUID.randomUUID(), "Inbox", "/Inbox", ROOT.id, role = Role.UPLOAD_ONLY)
        val mine = node("mine.pdf", parent = folder.id, role = Role.UPLOAD_ONLY)
        files.folders[folder.id] = folder
        files.children[folder.id] = mutableListOf(mine)
        val vm = viewModel(folder.id)
        assertTrue(vm.ui.value.mayAdd)
        assertTrue(vm.ui.value.onlyOwnUploads)
        vm.createFolder("Scans")
        vm.rename(mine, "b.pdf")
        vm.delete(mine)
        assertEquals(listOf("create Scans"), files.calls)
    }

    @Test
    fun `a move goes only into a folder the user may add to`() {
        val file = node("a.pdf").also { files.children.getValue(ROOT.id) += it }
        val vm = viewModel()
        val viewOnly = Folder(UUID.randomUUID(), "Shared", "/Shared", ROOT.id, role = Role.VIEWER)
        vm.move(file, viewOnly)
        assertEquals(emptyList<String>(), files.calls)
        vm.move(file, viewOnly.copy(role = Role.UPLOAD_ONLY))
        assertEquals(listOf("move a.pdf"), files.calls)
    }

    @Test
    fun `a failed action says why and keeps the list`() {
        val file = node("a.pdf").also { files.children.getValue(ROOT.id) += it }
        val vm = viewModel()
        files.fail = ApiException(409, "The file changed since that version")
        vm.delete(file)
        assertEquals("The file changed since that version", vm.ui.value.message)
        assertEquals(
            listOf("a.pdf"),
            vm.ui.value.items
                .map { it.name },
        )
    }

    @Test
    fun `opening a file downloads it, then hands it over once`() {
        val file = node("a.pdf")
        val vm = viewModel()
        vm.open(file)
        assertEquals(OpenFile(File("/cache/a.pdf"), "application/pdf"), vm.ui.value.toOpen)
        assertNull(vm.ui.value.opening)
        vm.opened()
        assertNull(vm.ui.value.toOpen)
    }

    @Test
    fun `this folder's uploads show, and the folder reloads when one completes`() {
        val vm = viewModel()
        val id = UUID.randomUUID()
        val running = UploadInfo(id, "video.mp4", ROOT.id.toString(), UploadInfo.State.RUNNING, 5, 10, null)
        val elsewhere = running.copy(id = UUID.randomUUID(), folderId = UUID.randomUUID().toString())
        uploads.value = listOf(running, elsewhere)
        assertEquals(listOf(running), vm.ui.value.uploads)

        val before = files.loads
        uploads.value = listOf(running.copy(state = UploadInfo.State.DONE))
        assertEquals(emptyList<UploadInfo>(), vm.ui.value.uploads)
        assertEquals(before + 1, files.loads)
    }

    @Test
    fun `a failed upload can be dismissed`() {
        val vm = viewModel()
        val failed = UploadInfo(UUID.randomUUID(), "x", ROOT.id.toString(), UploadInfo.State.FAILED, 0, 0, "Too large")
        uploads.value = listOf(failed)
        vm.dismissUpload(failed.id)
        assertEquals(emptyList<UploadInfo>(), vm.ui.value.uploads)
        uploads.value = listOf(failed)
        assertEquals(emptyList<UploadInfo>(), vm.ui.value.uploads)
    }
}
