package eu.vardalabs.organizer.share

import eu.vardalabs.organizer.FakeUnlocker
import eu.vardalabs.organizer.MainDispatcherRule
import eu.vardalabs.organizer.ROOT
import eu.vardalabs.organizer.api.models.Links
import eu.vardalabs.organizer.api.models.ReadContents
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.settings
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.io.IOException
import java.time.LocalDateTime
import java.util.UUID

class ShareViewModelTest {
    @get:Rule val main = MainDispatcherRule()

    private val sent = mutableListOf<ShareChoice>()
    private val encryption = FakeUnlocker()

    private fun viewModel(
        readContents: ReadContents? = ReadContents.PER_ITEM,
        hasLink: Boolean = false,
        send: suspend (ShareChoice) -> Unit = { sent += it },
    ) = ShareViewModel(
        hasLink = hasLink,
        settings = {
            readContents?.let { settings(readContents = it, links = Links.SNAPSHOT) }
                ?: throw IOException("offline")
        },
        unlocked = encryption::isUnlocked,
        send = send,
    )

    private val encrypted =
        UUID.randomUUID().let { Folder(it, "Private", "/Private", ROOT.id, e2eRoot = it, role = Role.OWNER) }

    @Test
    fun `an encrypted folder is refused while locked`() {
        val vm = viewModel()
        vm.onMode(ShareMode.FOLDER)
        vm.onFolder(encrypted)
        assertEquals(ShareViewModel.LOCKED, vm.ui.value.error)
        vm.onSend()
        assertTrue(sent.isEmpty())
        assertFalse(vm.ui.value.sending)
        assertFalse(vm.ui.value.done)
    }

    @Test
    fun `an encrypted folder takes what is shared once unlocked`() {
        encryption.unlocked.value = true
        val vm = viewModel()
        vm.onMode(ShareMode.FOLDER)
        vm.onFolder(encrypted)
        assertNull(vm.ui.value.error)
        vm.onSend()
        assertEquals(encrypted, sent.single().folder)
        assertTrue(vm.ui.value.done)
    }

    @Test
    fun `read contents is offered only where the settings leave it to each item`() {
        assertFalse(viewModel(ReadContents.ALWAYS).ui.value.offerReadContents)
        viewModel(ReadContents.PER_ITEM).ui.value.let {
            assertTrue(it.offerReadContents)
            assertFalse(it.readContents)
        }
        viewModel(ReadContents.DEFAULT_ON).ui.value.let {
            assertTrue(it.offerReadContents)
            assertTrue(it.readContents)
        }
        // Settings out of reach: no options, the user's settings apply.
        assertFalse(viewModel(null).ui.value.offerReadContents)
    }

    @Test
    fun `the drop box sends the note and options`() {
        val vm = viewModel(ReadContents.DEFAULT_ON)
        vm.onNote("  tax 2025  ")
        vm.onReadContents(false)
        vm.onSend()
        assertEquals(ShareChoice(ShareMode.DROP_BOX, "tax 2025", false, null, null), sent.single())
        assertTrue(vm.ui.value.done)
    }

    @Test
    fun `a link's mode is offered, and sent only when changed`() {
        val vm = viewModel(hasLink = true)
        assertTrue(vm.ui.value.offerLinkMode)
        assertEquals(Links.SNAPSHOT, vm.ui.value.linkMode)
        vm.onSend()
        assertNull(sent.last().linkMode)

        val changed = viewModel(hasLink = true)
        changed.onLinkMode(Links.DOWNLOAD)
        changed.onSend()
        assertEquals(Links.DOWNLOAD, sent.last().linkMode)
    }

    @Test
    fun `placing manually needs a folder`() {
        val vm = viewModel()
        vm.onMode(ShareMode.FOLDER)
        assertFalse(vm.ui.value.canSend)
        vm.onSend()
        assertTrue(sent.isEmpty())
        vm.onFolder(ROOT)
        vm.onSend()
        assertEquals(ShareMode.FOLDER, sent.single().mode)
        assertEquals(ROOT, sent.single().folder)
    }

    @Test
    fun `a view-only folder takes nothing`() {
        val vm = viewModel()
        vm.onMode(ShareMode.FOLDER)
        vm.onFolder(ROOT.copy(role = Role.VIEWER))
        assertFalse(vm.ui.value.canSend)
        vm.onSend()
        assertTrue(sent.isEmpty())
        vm.onFolder(ROOT.copy(role = Role.UPLOAD_ONLY))
        assertTrue(vm.ui.value.canSend)
    }

    @Test
    fun `a note is at most 2,000 characters`() {
        val vm = viewModel()
        vm.onNote("x".repeat(3_000))
        assertEquals(2_000, vm.ui.value.note.length)
    }

    @Test
    fun `a failure to send is said, and nothing finishes`() {
        val vm = viewModel(send = { throw IOException("Cannot read the file") })
        vm.onSend()
        assertEquals("Cannot read the file", vm.ui.value.error)
        assertFalse(vm.ui.value.done)
        assertFalse(vm.ui.value.sending)
    }
}

class SharedItemTest {
    @Test
    fun `a link is found in shared text`() {
        assertEquals("https://example.com/a?b=1", SharedItem.linkIn("Look at this: https://example.com/a?b=1."))
        assertNull(SharedItem.linkIn("no link here"))
        assertEquals(SharedItem.Link("https://x.org", "Title"), SharedItem.ofText("https://x.org", "Title"))
        assertEquals(SharedItem.Text("just words", null), SharedItem.ofText("just words", " "))
    }

    @Test
    fun `names of links and notes placed by hand`() {
        assertEquals("example.com.url", ShareSender.linkFileName("https://example.com/page", null))
        assertEquals("A page about cats.url", ShareSender.linkFileName("https://x.org", "A page: about cats?"))
        assertEquals(
            "Note 2026-11-07 1432.txt",
            ShareSender.textFileName(null, LocalDateTime.of(2026, 11, 7, 14, 32)),
        )
        assertEquals("[InternetShortcut]\r\nURL=https://x.org\r\n", ShareSender.shortcut("https://x.org"))
    }
}
