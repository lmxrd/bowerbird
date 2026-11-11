package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.MirrorStatus
import eu.vardalabs.organizer.api.models.MirrorStopped
import eu.vardalabs.organizer.core.e2e.E2e
import eu.vardalabs.organizer.core.e2e.Keyring
import eu.vardalabs.organizer.drive
import eu.vardalabs.organizer.mirror
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.UUID

/** Mirrors as the app shows them (docs/M7D_DESIGN.md §5): read-only drives, labelled with their state. */
class MirrorsTest {
    @Test
    fun `a mirror is labelled as one, and no other drive`() {
        assertTrue(mirror("Team").isMirror)
        assertEquals("Team · mirror", mirror("Team").title)
        assertFalse(drive("Team").isMirror)
        assertEquals("Team", drive("Team").title)
        assertNull(drive("Team").mirrorLine)
    }

    @Test
    fun `a mirror says where it stands, by the status's code`() {
        assertEquals("Up to date", mirror("Team").mirrorLine)
        assertEquals("Changes wait for the next copy", mirror("Team", MirrorStatus.PENDING).mirrorLine)
        assertEquals("Copying…", mirror("Team", MirrorStatus.COPYING).mirrorLine)
        assertEquals(
            "Waiting for your storage to be unlocked",
            mirror("Team", MirrorStatus.WAITING_FOR_UNLOCK).mirrorLine,
        )
        assertEquals(
            "Copying fails; it is tried again in a few minutes",
            mirror("Team", MirrorStatus.FAILING).mirrorLine,
        )
        assertEquals(
            "Stopped: The drive was deleted",
            mirror("Team", MirrorStatus.STOPPED, MirrorStopped.DRIVE_DELETED).mirrorLine,
        )
        assertEquals(
            "Stopped: Mirroring shared drives is not allowed here",
            mirror("Team", MirrorStatus.STOPPED, MirrorStopped.SHARED_DRIVES_OFF).mirrorLine,
        )
    }

    @Test
    fun `a mirror opens, takes nothing new, and warns of no storage of the user's own`() {
        val team = mirror("Team")
        assertTrue(team.opens)
        assertFalse(team.takesNew)
        assertNull(team.ownerWarning)
        assertTrue(Folder(UUID.randomUUID(), "Team · mirror", "/", null, mirror = true).readOnly)
    }

    @Test
    fun `an encrypted folder in a mirror opens with its source's ids`() =
        runTest {
            val mirrorDrive = "mirror"
            val copy = UUID.randomUUID()
            val source = UUID.randomUUID()
            val asked = mutableListOf<UUID>()
            val sources =
                MirrorSources(
                    isMirror = { it == mirrorDrive },
                    sourceOf = { id ->
                        asked += id
                        if (id == copy) source else null
                    },
                )
            // Elsewhere a node is its own; nothing is asked.
            assertEquals(copy, sources.sealedWith("team", copy))
            assertTrue(asked.isEmpty())
            // In a mirror, the node it copies, asked once.
            assertEquals(source, sources.sealedWith(mirrorDrive, copy))
            assertEquals(source, sources.sealedWith(mirrorDrive, copy))
            assertEquals(listOf(copy), asked)
            // A name sealed for the source's folder opens with that id, not the copy's.
            val ring = Keyring(source, 1, mapOf(1 to E2e.random(32)))
            val sealed = ring.encryptName(source, "Taxes.pdf")
            assertEquals("Taxes.pdf", ring.decryptName(sources.sealedWith(mirrorDrive, copy), sealed))
            // A node without a source (none known) stays itself.
            val other = UUID.randomUUID()
            assertEquals(other, sources.sealedWith(mirrorDrive, other))
        }

    @Test
    fun `an encrypted folder from another server opens and is written with the owner's ids`() =
        runTest {
            val folder = UUID.randomUUID()
            val theirs = UUID.randomUUID()
            val made = UUID.randomUUID()
            val sourceOfMade = mutableListOf<UUID?>(null)
            val asked = mutableListOf<UUID>()
            val sources =
                MirrorSources(
                    isMirror = { false },
                    sourceOf = { id ->
                        asked += id
                        when (id) {
                            folder -> theirs
                            made -> sourceOfMade.last()
                            else -> null
                        }
                    },
                )
            // Not a mirror's drive, but a node from another server: its source.
            assertEquals(folder, sources.sealedWith("remote", folder))
            assertEquals(theirs, sources.sealedWith("remote", folder, remote = true))
            assertEquals(theirs, sources.sealingFor(folder))
            val ring = Keyring(theirs, 1, mapOf(1 to E2e.random(32)))
            val sealed = ring.encryptName(sources.sealingFor(folder), "Taxes.pdf")
            assertEquals("Taxes.pdf", ring.decryptName(theirs, sealed))

            // A folder made a moment ago: asked again each time, refused while it has none.
            try {
                sources.sealingFor(made)
                fail("Sealed under a folder the owner's server does not know yet")
            } catch (_: NotOnOwnersServerException) {
            }
            sourceOfMade += UUID.randomUUID()
            assertEquals(sourceOfMade.last(), sources.sealingFor(made))
            assertEquals(2, asked.count { it == made })
        }
}
