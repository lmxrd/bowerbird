package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.bucket
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.drive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A drive's storage as the app follows it (docs/M7C_DESIGN.md). */
class StorageTest {
    private val alices = bucket("Alice's bucket", owner = "alice@example.com")

    @Test
    fun `a drive on the server's disk is ready and says nothing`() {
        val team = drive("Team")
        assertEquals(DriveStorage.Ready, team.storage)
        assertNull(team.storageLine)
        assertNull(team.ownerWarning)
        assertTrue(team.opens)
        assertTrue(team.takesNew)
    }

    @Test
    fun `a locked drive says until whom, and offers nothing`() {
        val locked =
            drive("Alice's", backend = alices, unavailable = "Locked until alice@example.com signs in", locked = true)
        assertEquals(DriveStorage.Unavailable("Locked until alice@example.com signs in", locked = true), locked.storage)
        assertEquals("Locked until alice@example.com signs in", locked.storageLine)
        assertFalse(locked.opens)
        assertFalse(locked.takesNew)
    }

    @Test
    fun `a failing drive says why, and offers nothing`() {
        val failing = drive("S3", backend = bucket("S3"), unavailable = "The storage cannot be reached")
        assertEquals(DriveStorage.Unavailable("The storage cannot be reached", locked = false), failing.storage)
        assertFalse(failing.opens)
        assertNull(failing.ownerWarning)
    }

    @Test
    fun `a moving drive opens read-only and takes nothing new`() {
        val moving = drive("Team", movingTo = bucket("S3"))
        assertEquals(DriveStorage.Moving("S3"), moving.storage)
        assertEquals("Moving to S3…", moving.storageLine)
        assertTrue(moving.opens)
        assertFalse(moving.takesNew)
        assertEquals("Moving to S3… Read-only until that is done.", movingNote("S3"))
    }

    @Test
    fun `the server's refusals are told apart`() {
        val locked =
            ApiException.of(503, """{"error":"Locked until alice@example.com signs in","code":"storage_unavailable"}""")
        assertTrue(locked.storageUnavailable)
        assertTrue(locked.retryable)
        assertEquals("This drive is not available now: Locked until alice@example.com signs in.", locked.message)
        // A drive being moved: tried again later, never a version conflict.
        val moving =
            ApiException.of(
                409,
                """{"error":"This drive is being moved to other storage: it is read-only until that is done","code":"read_only"}""",
            )
        assertTrue(moving.readOnly)
        assertTrue(moving.retryable)
        // Told by its code, not by its words.
        assertFalse(ApiException.of(409, """{"error":"This drive is being moved"}""").readOnly)
        val conflict = ApiException.of(409, """{"error":"The file changed since that version"}""")
        assertFalse(conflict.readOnly)
        assertFalse(conflict.retryable)
        assertEquals("The server answered 502", ApiException.of(502, "<html>").message)
    }

    @Test
    fun `someone's own storage warns its members`() {
        assertEquals(
            "Stored on alice@example.com's own storage. If they remove it, this drive goes. Not backed up by the server.",
            drive("Alice's", backend = alices).ownerWarning,
        )
    }
}
