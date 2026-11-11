package eu.vardalabs.organizer.core.auth

import eu.vardalabs.organizer.api.apis.AuthApi
import eu.vardalabs.organizer.api.models.DeviceToken
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.api.GeneratedJson
import eu.vardalabs.organizer.core.api.apiCall
import eu.vardalabs.organizer.core.session.PendingSignIn
import eu.vardalabs.organizer.core.session.Session
import eu.vardalabs.organizer.core.session.SessionStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.IOException

/** The token the server issued for this device. */
data class IssuedToken(
    val token: String,
    val keyPrefix: String,
)

/** The server side of the device login, apart so that tests can stand in for it. */
interface DeviceLoginServer {
    /** Throws when the server cannot be reached or is not Organizer. */
    suspend fun check(server: HttpUrl)

    suspend fun exchange(
        server: HttpUrl,
        code: String,
        verifier: String,
    ): IssuedToken
}

/** [DeviceLoginServer] over HTTP: `GET /api/health`, `POST /api/auth/device/token`. */
class HttpDeviceLoginServer(
    private val http: OkHttpClient,
) : DeviceLoginServer {
    override suspend fun check(server: HttpUrl) {
        val request = Request.Builder().url(server.newBuilder().addPathSegments("api/health").build()).build()
        withContext(Dispatchers.IO) {
            http.newCall(request).execute().use {
                if (!it.isSuccessful) throw IOException("Not an Organizer server (${it.code})")
            }
        }
    }

    override suspend fun exchange(
        server: HttpUrl,
        code: String,
        verifier: String,
    ): IssuedToken {
        GeneratedJson.ensure()
        val created =
            apiCall {
                AuthApi(
                    DeviceLogin.baseUrl(server),
                    http,
                ).deviceToken(DeviceToken(code = code, codeVerifier = verifier))
            }
        return IssuedToken(created.token, created.key.prefix)
    }
}

/**
 * Signing in through the browser: [start] makes the PKCE pair, keeps the
 * verifier and answers the page to open; [complete] takes the redirect back
 * and exchanges its code for the token, which it saves.
 */
class DeviceSignIn(
    private val store: SessionStore,
    private val server: DeviceLoginServer,
    private val newVerifier: () -> String = { Pkce.verifier() },
) {
    /** Throws [IllegalArgumentException] for an address that is not one, [IOException] when unreachable. */
    suspend fun start(
        address: String,
        deviceName: String,
    ): HttpUrl {
        val url = DeviceLogin.normalizeServer(address) ?: throw IllegalArgumentException("Not a server address")
        server.check(url)
        val verifier = newVerifier()
        val name = DeviceLogin.deviceName(deviceName)
        store.savePending(PendingSignIn(DeviceLogin.baseUrl(url), verifier, name))
        return DeviceLogin.authorizeUrl(url, Pkce.challenge(verifier), name)
    }

    /** Finish with the redirect to `organizer://auth`; throws when it cannot. */
    suspend fun complete(redirect: String): Session {
        val code = DeviceLogin.codeFrom(redirect) ?: throw ApiException(400, "The sign-in did not return a code")
        val pending = store.takePending() ?: throw ApiException(400, "No sign-in was started: try again")
        val issued = server.exchange(pending.server.toHttpUrl(), code, pending.verifier)
        val session =
            Session(
                server = pending.server,
                token = issued.token,
                keyPrefix = issued.keyPrefix,
                deviceName = pending.deviceName,
            )
        store.save(session)
        return session
    }
}
