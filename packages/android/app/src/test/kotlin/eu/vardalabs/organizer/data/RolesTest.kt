package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Role
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RolesTest {
    private val none = ItemActions(rename = false, move = false, delete = false)
    private val all = ItemActions(rename = true, move = true, delete = true)

    @Test
    fun `a viewer only reads`() {
        assertFalse(Role.VIEWER.mayAdd)
        assertFalse(Role.VIEWER.mayChange)
        assertFalse(Role.VIEWER.onlyOwnUploads)
        assertEquals(none, ItemActions.of(Role.VIEWER))
        assertFalse(ItemActions.of(Role.VIEWER).any)
    }

    @Test
    fun `upload only adds, and changes nothing, not even what it added`() {
        assertTrue(Role.UPLOAD_ONLY.mayAdd)
        assertFalse(Role.UPLOAD_ONLY.mayChange)
        assertTrue(Role.UPLOAD_ONLY.onlyOwnUploads)
        assertEquals(none, ItemActions.of(Role.UPLOAD_ONLY))
    }

    @Test
    fun `an editor and above add, rename, move and delete`() {
        for (role in listOf(Role.EDITOR, Role.MANAGER, Role.OWNER)) {
            assertTrue(role.mayAdd)
            assertTrue(role.mayChange)
            assertFalse(role.onlyOwnUploads)
            assertEquals(all, ItemActions.of(role))
            assertTrue(ItemActions.of(role).any)
        }
    }

    @Test
    fun `in an encrypted folder a move is not offered`() {
        assertEquals(all.copy(move = false), ItemActions.of(Role.OWNER, sealed = true))
    }

    @Test
    fun `no role allows nothing but reading`() {
        val role: Role? = null
        assertFalse(role.mayAdd)
        assertFalse(role.mayChange)
        assertEquals(none, ItemActions.of(null))
    }
}
