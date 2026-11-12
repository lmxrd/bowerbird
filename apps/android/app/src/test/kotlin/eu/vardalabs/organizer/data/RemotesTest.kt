package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.IncomingShareState
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.bucket
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.drive
import eu.vardalabs.organizer.fromAlice
import eu.vardalabs.organizer.node
import eu.vardalabs.organizer.remoteDrive
import eu.vardalabs.organizer.ui.drives.shared
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Shares from other servers as the app shows them (docs/M7E_DESIGN.md §5, §6). */
class RemotesTest {
    @Test
    fun `a remote drive says whose it is and where, and no other drive is one`() {
        val theirs = remoteDrive("Research")
        assertTrue(theirs.isRemote)
        assertFalse(drive("Team").isRemote)
        assertEquals("Research", theirs.title)
        assertEquals("From alice@cloud.example", theirs.remote?.fromLine)
        // The server named apart when the address does not say it.
        assertEquals(
            "From alice on files.example",
            fromAlice().copy(owner = "alice", server = "files.example").fromLine,
        )
    }

    @Test
    fun `a remote drive says how it stands, by its state`() {
        assertNull(remoteDrive("Research").remote?.stateLine)
        assertEquals(
            "Waiting for you to accept it",
            remoteDrive("Research", fromAlice(IncomingShareState.PENDING)).remote?.stateLine,
        )
        assertEquals(
            "Its server cannot be reached now: connection refused",
            remoteDrive("Research", fromAlice(error = "connection refused")).remote?.stateLine,
        )
    }

    @Test
    fun `a remote drive opens once accepted, follows its role, and never takes anything new`() {
        val pending = remoteDrive("Research", fromAlice(IncomingShareState.PENDING))
        assertFalse(pending.opens)
        val accepted = remoteDrive("Research", role = Role.VIEWER)
        assertTrue(accepted.opens)
        assertFalse(accepted.takesNew)
        assertEquals(Role.VIEWER, accepted.myRole)
        assertNull(remoteDrive("Research").copy(backend = bucket("Theirs", owner = "alice")).ownerWarning)
    }

    @Test
    fun `shared with me marks what came from another server, and what waits for an answer`() {
        val local = shared(node("Plans", Kind.FOLDER))
        assertFalse(local.awaitsAnswer)
        assertEquals("From kari@example.com", local.fromLine)
        val pending =
            shared(node("Plans", Kind.FOLDER, remote = "cloud.example"), fromAlice(IncomingShareState.PENDING))
        assertTrue(pending.awaitsAnswer)
        assertEquals("From alice@cloud.example", pending.fromLine)
        assertFalse(shared(node("Plans", Kind.FOLDER, remote = "cloud.example"), fromAlice()).awaitsAnswer)
    }

    @Test
    fun `refusals about another server are told by their code`() {
        val unreachable = ApiException.of(502, """{"error": "dial tcp: refused", "code": "remote_unreachable"}""")
        assertTrue(unreachable.remoteUnreachable)
        assertTrue(unreachable.retryable)
        assertEquals("The other server cannot be reached now. Try again later.", unreachable.message)
        val refused = ApiException.of(502, """{"error": "403 from the other server", "code": "remote_refused"}""")
        assertFalse(refused.retryable)
        assertEquals("The other server refused this.", refused.message)
        // One the app does not meet keeps the server's words.
        val contact = ApiException.of(403, """{"error": "Invite them first", "code": "contact_required"}""")
        assertEquals("Invite them first", contact.message)
        assertEquals("contact_required", contact.code)
    }
}
