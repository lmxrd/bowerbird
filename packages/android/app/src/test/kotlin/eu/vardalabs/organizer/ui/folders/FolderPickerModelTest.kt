package eu.vardalabs.organizer.ui.folders

import eu.vardalabs.organizer.MY
import eu.vardalabs.organizer.ROOT
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.bucket
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.drive
import eu.vardalabs.organizer.mirror
import eu.vardalabs.organizer.node
import eu.vardalabs.organizer.remoteDrive
import eu.vardalabs.organizer.ui.files.FakeFiles
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.UUID

@OptIn(ExperimentalCoroutinesApi::class)
class FolderPickerModelTest {
    private val scope = CoroutineScope(UnconfinedTestDispatcher())
    private val files = FakeFiles()

    /** A shared drive "Team" with a folder "Plans" in it. */
    private val team = drive("Team")
    private val teamRoot = Folder(team.rootId!!, "Team", "/", null, driveName = "Team", role = Role.EDITOR)
    private val plans = node("Plans", Kind.FOLDER, parent = teamRoot.id, driveId = team.id)
    private val plansFolder = Folder(plans.id, "Plans", "/Plans", teamRoot.id, driveName = "Team", role = Role.EDITOR)
    private val unavailable = drive("Archive", rootId = null)

    init {
        files.folders[teamRoot.id] = teamRoot
        files.folders[plans.id] = plansFolder
        files.children[teamRoot.id] = mutableListOf(plans, node("notes.txt", parent = teamRoot.id, driveId = team.id))
        files.children[plans.id] = mutableListOf()
        files.children.getValue(ROOT.id) += node("Photos", Kind.FOLDER)
    }

    private fun model(
        start: UUID? = null,
        exclude: UUID? = null,
        allowRoot: Boolean = true,
        drives: (suspend () -> List<Drive>)? = { listOf(MY, team, unavailable) },
    ) = FolderPickerModel(files, drives, start, exclude, allowRoot, scope)

    @Test
    fun `it starts at the drives, those whose folder is there`() {
        val picker = model()
        val state = picker.ui.value
        assertEquals(listOf("My drive", "Team"), state.drives?.map { it.name })
        // Nor one whose storage is unavailable, nor one being moved (read-only).
        val locked = drive("Locked", unavailable = "Locked until alice@example.com signs in", locked = true)
        val moving = drive("Moving", movingTo = bucket("S3"))
        val more = model(drives = { listOf(MY, team, locked, moving) })
        assertEquals(
            listOf("My drive", "Team"),
            more.ui.value.drives
                ?.map { it.name },
        )
        // A folder of a drive being moved is not chosen.
        val movingRoot = teamRoot.copy(id = UUID.randomUUID(), movingTo = "S3")
        files.folders[movingRoot.id] = movingRoot
        files.children[movingRoot.id] = mutableListOf()
        more.open(movingRoot.id)
        assertFalse(more.ui.value.canPick)
        assertNull(state.folder)
        assertFalse(state.canPick)
        assertFalse(state.canGoUp)
    }

    @Test
    fun `a mirror is never offered, nor a folder in one chosen`() {
        val picker = model(drives = { listOf(MY, team, mirror("Team")) })
        assertEquals(
            listOf("My drive", "Team"),
            picker.ui.value.drives
                ?.map { it.name },
        )
        // Even reached by id, a mirror's folder is read-only.
        val copy = teamRoot.copy(id = UUID.randomUUID(), driveName = "Team · mirror", mirror = true)
        files.folders[copy.id] = copy
        files.children[copy.id] = mutableListOf()
        picker.open(copy.id)
        assertFalse(picker.ui.value.canPick)
    }

    @Test
    fun `nothing new is put on another server, but a move in a remote drive follows the role`() {
        val theirs = remoteDrive("Research")
        val picker = model(drives = { listOf(MY, team, theirs) })
        assertEquals(
            listOf("My drive", "Team"),
            picker.ui.value.drives
                ?.map { it.name },
        )
        // A share from another server placed in My drive: neither listed nor chosen.
        val placed = node("Shared plans", Kind.FOLDER, remote = "cloud.example")
        files.children.getValue(ROOT.id) += placed
        val placedFolder =
            Folder(placed.id, "Shared plans", "/Shared plans", ROOT.id, role = Role.EDITOR, remote = "cloud.example")
        files.folders[placed.id] = placedFolder
        files.children[placed.id] = mutableListOf()
        picker.open(ROOT.id)
        assertEquals(
            listOf("Photos"),
            picker.ui.value.children
                .map { it.name },
        )
        picker.open(placed.id)
        assertFalse(picker.ui.value.canPick)

        // A move inside it: an editor's, chosen.
        val sub = node("Sub", Kind.FOLDER, parent = placed.id, remote = "cloud.example")
        files.children[placed.id] = mutableListOf(sub)
        val move = model(start = placed.id, exclude = sub.id, drives = null)
        assertTrue(move.ui.value.canPick)
    }

    @Test
    fun `a drive opens at its root, which goes up to the drives`() {
        val picker = model()
        picker.openDrive(team)
        val state = picker.ui.value
        assertNull(state.drives)
        assertEquals("Team", state.folder?.location)
        // Folders only.
        assertEquals(listOf("Plans"), state.children.map { it.name })
        assertTrue(state.canPick)
        assertTrue(state.canGoUp)

        picker.open(plans.id)
        assertEquals(
            "Team/Plans",
            picker.ui.value.folder
                ?.location,
        )
        picker.up()
        assertEquals(teamRoot, picker.ui.value.folder)
        picker.up()
        assertEquals(
            listOf("My drive", "Team"),
            picker.ui.value.drives
                ?.map { it.name },
        )
    }

    @Test
    fun `a drive's root is not chosen where folders only may be`() {
        val picker = model(allowRoot = false)
        picker.openDrive(team)
        assertFalse(picker.ui.value.canPick)
        picker.open(plans.id)
        assertTrue(picker.ui.value.canPick)
    }

    @Test
    fun `a move starts in its folder and stays in its drive`() {
        val moved = node("Old", Kind.FOLDER, parent = plans.id, driveId = team.id)
        files.children[plans.id] = mutableListOf(moved)
        val picker = model(start = plans.id, exclude = moved.id, drives = null)
        assertEquals(plansFolder, picker.ui.value.folder)
        // The folder being moved is neither shown nor entered.
        assertTrue(
            picker.ui.value.children
                .isEmpty(),
        )
        picker.open(moved.id)
        assertEquals(plansFolder, picker.ui.value.folder)
        picker.up()
        assertEquals(teamRoot, picker.ui.value.folder)
        // The drive's root is the top: no drives above it.
        assertFalse(picker.ui.value.canGoUp)
        picker.up()
        assertEquals(teamRoot, picker.ui.value.folder)
    }

    @Test
    fun `only a folder the user may add to is chosen, and a view-only one is still entered`() {
        val viewOnly = node("Reports", Kind.FOLDER, parent = teamRoot.id, driveId = team.id, role = Role.VIEWER)
        val inbox = node("Inbox", Kind.FOLDER, parent = viewOnly.id, driveId = team.id, role = Role.UPLOAD_ONLY)
        files.folders[viewOnly.id] =
            Folder(viewOnly.id, "Reports", "/Reports", teamRoot.id, driveName = "Team", role = Role.VIEWER)
        files.folders[inbox.id] =
            Folder(inbox.id, "Inbox", "/Reports/Inbox", viewOnly.id, driveName = "Team", role = Role.UPLOAD_ONLY)
        files.children[viewOnly.id] = mutableListOf(inbox)
        files.children[inbox.id] = mutableListOf()
        val picker = model()
        picker.open(viewOnly.id)
        assertFalse(picker.ui.value.canPick)
        assertTrue(picker.ui.value.viewOnly)
        assertEquals(
            listOf("Inbox"),
            picker.ui.value.children
                .map { it.name },
        )
        // Given more below: upload only may be added to.
        picker.open(inbox.id)
        assertTrue(picker.ui.value.canPick)
        assertFalse(picker.ui.value.viewOnly)
    }

    @Test
    fun `drives that cannot be read say why`() {
        val picker = model(drives = { throw ApiException(500, "The server is down") })
        assertEquals("The server is down", picker.ui.value.error)
        assertFalse(picker.ui.value.loading)
    }
}
