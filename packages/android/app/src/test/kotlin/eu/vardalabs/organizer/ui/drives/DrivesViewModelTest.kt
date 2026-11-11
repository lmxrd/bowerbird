package eu.vardalabs.organizer.ui.drives

import eu.vardalabs.organizer.MY
import eu.vardalabs.organizer.MainDispatcherRule
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.IncomingShareState
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.api.models.Perms
import eu.vardalabs.organizer.api.models.Person
import eu.vardalabs.organizer.api.models.RemoteDrive
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.api.models.SharedItem
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.data.DrivesRepository
import eu.vardalabs.organizer.data.awaitsAnswer
import eu.vardalabs.organizer.data.fromLine
import eu.vardalabs.organizer.data.opens
import eu.vardalabs.organizer.drive
import eu.vardalabs.organizer.fromAlice
import eu.vardalabs.organizer.mirror
import eu.vardalabs.organizer.node
import eu.vardalabs.organizer.remoteDrive
import eu.vardalabs.organizer.ui.files.OpenFile
import kotlinx.coroutines.flow.MutableSharedFlow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.util.UUID

class FakeDrives : DrivesRepository {
    var drives = listOf(MY)
    var shared = emptyList<SharedItem>()
    var fail: ApiException? = null
    var loads = 0

    override suspend fun drives(): List<Drive> {
        loads++
        fail?.let { throw it }
        return drives
    }

    override suspend fun sharedWithMe(): List<SharedItem> = shared

    val accepted = mutableListOf<UUID>()
    val declined = mutableListOf<UUID>()

    /** What another server shared, accepted: it stands accepted. */
    override suspend fun accept(id: UUID) {
        fail?.let { throw it }
        accepted += id
        shared =
            shared.map {
                if (it.node.id ==
                    id
                ) {
                    it.copy(remote = it.remote?.copy(state = IncomingShareState.ACCEPTED))
                } else {
                    it
                }
            }
        drives =
            drives.map {
                if (it.rootId ==
                    id
                ) {
                    it.copy(remote = it.remote?.copy(state = IncomingShareState.ACCEPTED))
                } else {
                    it
                }
            }
    }

    /** Declined: gone. */
    override suspend fun decline(id: UUID) {
        fail?.let { throw it }
        declined += id
        shared = shared.filterNot { it.node.id == id }
        drives = drives.filterNot { it.rootId == id }
    }
}

fun shared(
    node: Node,
    remote: RemoteDrive? = null,
) = SharedItem(
    remote = remote,
    group = null,
    new = false,
    node = node,
    owner = Person(email = "kari@example.com", id = UUID.randomUUID()),
    permissions = Perms(),
    placed = null,
    role = Role.VIEWER,
    sharedAt = "2026-11-08T09:00:00Z",
    sharedBy = Person(email = "kari@example.com", id = UUID.randomUUID()),
)

class DrivesViewModelTest {
    @get:Rule val main = MainDispatcherRule()

    private val repository = FakeDrives()
    private val changes = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private val downloaded = mutableListOf<String>()

    private fun viewModel() =
        DrivesViewModel(repository, changes) { node ->
            downloaded += node.name
            File("/cache/${node.name}")
        }

    @Test
    fun `My drive first, then the shared drives and what is shared`() {
        val team = drive("Team", members = 4)
        val plans = shared(node("Plans", Kind.FOLDER, driveId = "another"))
        repository.drives = listOf(MY, team)
        repository.shared = listOf(plans)
        val vm = viewModel()
        val state = vm.ui.value
        assertFalse(state.loading)
        assertNull(state.error)
        assertEquals(listOf("My drive", "Team"), state.drives.map { it.name })
        assertEquals(listOf("Plans"), state.shared.map { it.node.name })
    }

    @Test
    fun `the mirrors come in a group of their own, after the shared drives`() {
        val team = drive("Team")
        val copy = mirror("Team")
        repository.drives = listOf(MY, team, copy)
        val state = viewModel().ui.value
        assertEquals(listOf(MY, team), state.ownAndShared)
        assertEquals(listOf(copy), state.mirrors)
    }

    @Test
    fun `the drives of other servers come last, in a group of their own`() {
        val team = drive("Team")
        val copy = mirror("Team")
        val theirs = remoteDrive("Research")
        repository.drives = listOf(MY, team, copy, theirs)
        val state = viewModel().ui.value
        assertEquals(listOf(MY, team), state.ownAndShared)
        assertEquals(listOf(copy), state.mirrors)
        assertEquals(listOf(theirs), state.remotes)
    }

    @Test
    fun `a remote drive waiting is accepted by its root, then opens`() {
        val root = UUID.randomUUID()
        repository.drives = listOf(MY, remoteDrive("Research", fromAlice(IncomingShareState.PENDING), rootId = root))
        val vm = viewModel()
        assertFalse(
            vm.ui.value.remotes
                .single()
                .opens,
        )
        vm.accept(root)
        assertEquals(listOf(root), repository.accepted)
        assertTrue(
            vm.ui.value.remotes
                .single()
                .opens,
        )
        assertNull(vm.ui.value.answering)
    }

    @Test
    fun `what another server shared is accepted or declined, and the list read again`() {
        val plans =
            shared(
                node("Plans", Kind.FOLDER, driveId = "theirs", remote = "cloud.example"),
                fromAlice(IncomingShareState.PENDING),
            )
        val notes =
            shared(
                node("notes.txt", driveId = "theirs", remote = "cloud.example"),
                fromAlice(IncomingShareState.PENDING),
            )
        repository.shared = listOf(plans, notes)
        val vm = viewModel()
        assertTrue(
            vm.ui.value.shared
                .all { it.awaitsAnswer },
        )
        assertEquals("From alice@cloud.example", plans.fromLine)

        vm.accept(plans.node.id)
        vm.decline(notes.node.id)
        assertEquals(listOf(plans.node.id), repository.accepted)
        assertEquals(listOf(notes.node.id), repository.declined)
        val left =
            vm.ui.value.shared
                .single()
        assertEquals("Plans", left.node.name)
        assertFalse(left.awaitsAnswer)

        // A refusal says so, by the code's words.
        repository.fail = ApiException.of(502, """{"error": "connection refused", "code": "remote_unreachable"}""")
        vm.decline(plans.node.id)
        assertEquals("The other server cannot be reached now. Try again later.", vm.ui.value.message)
        assertNull(vm.ui.value.answering)
    }

    @Test
    fun `a drive joined shows when the live events say something changed`() {
        val vm = viewModel()
        assertEquals(1, vm.ui.value.drives.size)
        repository.drives = listOf(MY, drive("Team"))
        changes.tryEmit(Unit)
        assertEquals(
            listOf("My drive", "Team"),
            vm.ui.value.drives
                .map { it.name },
        )
    }

    @Test
    fun `drives that cannot be read say why, and a failed refresh keeps them`() {
        repository.fail = ApiException(500, "The server is down")
        val vm = viewModel()
        assertEquals("The server is down", vm.ui.value.error)

        repository.fail = null
        vm.refresh()
        assertNull(vm.ui.value.error)
        repository.fail = ApiException(500, "Offline")
        vm.refresh()
        assertEquals(
            listOf("My drive"),
            vm.ui.value.drives
                .map { it.name },
        )
        assertNull(vm.ui.value.error)
        assertEquals("Offline", vm.ui.value.message)
        assertFalse(vm.ui.value.refreshing)
    }

    @Test
    fun `a shared file is downloaded to open`() {
        val file = node("a.pdf")
        val vm = viewModel()
        vm.open(file)
        assertEquals(listOf("a.pdf"), downloaded)
        assertEquals(OpenFile(File("/cache/a.pdf"), "application/pdf"), vm.ui.value.toOpen)
        assertNull(vm.ui.value.opening)
    }

    @Test
    fun `a shared drive shows its members and size`() {
        assertEquals("1 member · 2.0 KB", membersAndSize(1, "2.0 KB"))
        assertEquals("4 members · 2.0 KB", membersAndSize(4, "2.0 KB"))
    }
}
