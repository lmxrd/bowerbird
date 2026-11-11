package eu.vardalabs.organizer.core.auth

import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64

/** PKCE (RFC 7636) with S256, the only method the server accepts. */
object Pkce {
    private val base64url = Base64.getUrlEncoder().withoutPadding()

    /** A new verifier: 32 random bytes, 43 characters of base64url. */
    fun verifier(random: SecureRandom = SecureRandom()): String {
        val bytes = ByteArray(32)
        random.nextBytes(bytes)
        return base64url.encodeToString(bytes)
    }

    /** The challenge of a verifier: base64url(SHA-256(verifier)). */
    fun challenge(verifier: String): String =
        base64url.encodeToString(MessageDigest.getInstance("SHA-256").digest(verifier.toByteArray(Charsets.US_ASCII)))
}
