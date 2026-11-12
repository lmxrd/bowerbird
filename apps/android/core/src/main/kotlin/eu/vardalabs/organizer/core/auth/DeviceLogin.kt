package eu.vardalabs.organizer.core.auth

import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import java.net.URI
import java.net.URLDecoder

/**
 * The pieces of the device login (docs/ARCHITECTURE.md §4.4): the app opens
 * the server's authorize page in a Custom Tab with a PKCE challenge, the
 * server sends the browser back to [REDIRECT_URI] with a one-time code, and
 * the app exchanges the code and its verifier for the device's token.
 */
object DeviceLogin {
    /** The app's redirect, on the server's allow-list. */
    const val REDIRECT_URI = "organizer://auth"

    /**
     * The server's base URL from what someone typed: https unless they wrote
     * http://, without a trailing slash, query or fragment. Null when it is
     * not an address.
     */
    fun normalizeServer(input: String): HttpUrl? {
        val trimmed = input.trim()
        if (trimmed.isEmpty() || trimmed.any { it.isWhitespace() }) return null
        val withScheme = if ("://" in trimmed) trimmed else "https://$trimmed"
        val url = withScheme.toHttpUrlOrNull() ?: return null
        if (url.host.isEmpty()) return null
        val segments = url.pathSegments.filter { it.isNotEmpty() }
        return url
            .newBuilder()
            .query(null)
            .fragment(null)
            .encodedPath("/")
            .apply { segments.forEach { addPathSegment(it) } }
            .build()
    }

    /** The server's base URL as a string the generated client takes (no trailing slash). */
    fun baseUrl(server: HttpUrl): String = server.toString().trimEnd('/')

    /** The page to open in the Custom Tab. */
    fun authorizeUrl(
        server: HttpUrl,
        challenge: String,
        deviceName: String,
    ): HttpUrl =
        server
            .newBuilder()
            .addPathSegments("api/auth/device/authorize")
            .addQueryParameter("redirect_uri", REDIRECT_URI)
            .addQueryParameter("code_challenge", challenge)
            .addQueryParameter("code_challenge_method", "S256")
            .addQueryParameter("device_name", deviceName)
            .build()

    /** The one-time code of a redirect to the app, or null when it is not one. */
    fun codeFrom(redirect: String): String? {
        val uri = runCatching { URI(redirect) }.getOrNull() ?: return null
        if (uri.scheme != "organizer" || uri.host != "auth") return null
        val query = uri.rawQuery ?: return null
        return query
            .split('&')
            .map { it.split('=', limit = 2) }
            .firstOrNull { it.size == 2 && it[0] == "code" }
            ?.let { URLDecoder.decode(it[1], "UTF-8") }
            ?.takeIf { it.isNotEmpty() }
    }

    /** A device name the server accepts: 1 to 100 characters. */
    fun deviceName(raw: String?): String = raw?.trim()?.take(100)?.ifEmpty { null } ?: "Android"
}
