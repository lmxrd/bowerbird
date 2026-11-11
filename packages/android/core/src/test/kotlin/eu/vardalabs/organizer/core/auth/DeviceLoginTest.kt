package eu.vardalabs.organizer.core.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.security.SecureRandom

class DeviceLoginTest {
    @Test
    fun `the challenge follows RFC 7636`() {
        // The example of RFC 7636, appendix B (the backend's test has the same).
        assertEquals(
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            Pkce.challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        )
    }

    @Test
    fun `a verifier is 43 characters of base64url, new each time`() {
        val random = SecureRandom()
        val a = Pkce.verifier(random)
        assertEquals(43, a.length)
        assert(a.all { it.isLetterOrDigit() || it == '-' || it == '_' })
        assertNotEquals(a, Pkce.verifier(random))
    }

    @Test
    fun `a server address is https unless it says otherwise`() {
        assertEquals("https://files.example.com/", DeviceLogin.normalizeServer(" files.example.com ").toString())
        assertEquals("https://files.example.com/", DeviceLogin.normalizeServer("https://files.example.com/").toString())
        assertEquals("http://192.168.1.5:8000/", DeviceLogin.normalizeServer("http://192.168.1.5:8000").toString())
        assertEquals(
            "https://example.com/organizer",
            DeviceLogin.normalizeServer("example.com/organizer/?x=1#y").toString(),
        )
        assertNull(DeviceLogin.normalizeServer(""))
        assertNull(DeviceLogin.normalizeServer("not an address"))
        assertNull(DeviceLogin.normalizeServer("ftp://example.com"))
    }

    @Test
    fun `the base URL has no trailing slash`() {
        assertEquals("https://example.com", DeviceLogin.baseUrl(DeviceLogin.normalizeServer("example.com")!!))
        assertEquals("https://example.com/o", DeviceLogin.baseUrl(DeviceLogin.normalizeServer("example.com/o/")!!))
    }

    @Test
    fun `the authorize page carries the redirect, the challenge and the device`() {
        val url = DeviceLogin.authorizeUrl(DeviceLogin.normalizeServer("example.com")!!, "abc", "Anna's Pixel")
        assertEquals("/api/auth/device/authorize", url.encodedPath)
        assertEquals("organizer://auth", url.queryParameter("redirect_uri"))
        assertEquals("abc", url.queryParameter("code_challenge"))
        assertEquals("S256", url.queryParameter("code_challenge_method"))
        assertEquals("Anna's Pixel", url.queryParameter("device_name"))
    }

    @Test
    fun `the code comes only from the app's redirect`() {
        assertEquals("f00d", DeviceLogin.codeFrom("organizer://auth?code=f00d"))
        assertEquals("a b", DeviceLogin.codeFrom("organizer://auth?state=x&code=a%20b"))
        assertNull(DeviceLogin.codeFrom("organizer://other?code=f00d"))
        assertNull(DeviceLogin.codeFrom("https://auth?code=f00d"))
        assertNull(DeviceLogin.codeFrom("organizer://auth"))
        assertNull(DeviceLogin.codeFrom("organizer://auth?code="))
    }

    @Test
    fun `a device name is 1 to 100 characters`() {
        assertEquals("Android", DeviceLogin.deviceName("  "))
        assertEquals("Android", DeviceLogin.deviceName(null))
        assertEquals(100, DeviceLogin.deviceName("x".repeat(300)).length)
    }
}
