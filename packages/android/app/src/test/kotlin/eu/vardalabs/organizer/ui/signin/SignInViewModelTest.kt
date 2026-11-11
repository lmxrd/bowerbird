package eu.vardalabs.organizer.ui.signin

import eu.vardalabs.organizer.MainDispatcherRule
import eu.vardalabs.organizer.core.api.ApiException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import java.io.IOException

class SignInViewModelTest {
    @get:Rule val main = MainDispatcherRule()

    private val completed = mutableListOf<String>()

    private fun viewModel(
        start: suspend (String, String) -> String = { a, d -> "https://$a/api/auth/device/authorize?device_name=$d" },
        complete: suspend (String) -> Unit = { completed += it },
        last: String? = null,
    ) = SignInViewModel("Pixel 9", start, complete, { last })

    @Test
    fun `the last server fills the address`() {
        assertEquals("files.example.com", viewModel(last = "files.example.com").ui.value.address)
    }

    @Test
    fun `continue answers the page to open, once`() {
        val vm = viewModel()
        vm.onAddress("example.com")
        vm.onContinue()
        assertEquals("https://example.com/api/auth/device/authorize?device_name=Pixel 9", vm.ui.value.openUrl)
        vm.onOpened()
        assertNull(vm.ui.value.openUrl)
    }

    @Test
    fun `an address that is not one, or a server out of reach, is said`() {
        val invalid = viewModel(start = { _, _ -> throw IllegalArgumentException() })
        invalid.onAddress("not an address")
        invalid.onContinue()
        assertEquals("That is not a server address", invalid.ui.value.error)
        assertFalse(invalid.ui.value.busy)

        val unreachable = viewModel(start = { _, _ -> throw IOException("timeout") })
        unreachable.onAddress("example.com")
        unreachable.onContinue()
        assertEquals("Cannot reach the server: timeout", unreachable.ui.value.error)
    }

    @Test
    fun `the redirect completes the sign-in`() {
        val vm = viewModel()
        vm.onRedirect("organizer://auth?code=c0de")
        assertEquals(listOf("organizer://auth?code=c0de"), completed)
        assertFalse(vm.ui.value.busy)
    }

    @Test
    fun `a refused code is said`() {
        val vm = viewModel(complete = { throw ApiException(400, "Invalid or expired code") })
        vm.onRedirect("organizer://auth?code=old")
        assertEquals("The sign-in failed: Invalid or expired code", vm.ui.value.error)
    }
}
