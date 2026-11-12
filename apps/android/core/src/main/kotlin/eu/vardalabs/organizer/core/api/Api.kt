package eu.vardalabs.organizer.core.api

import eu.vardalabs.organizer.api.apis.AuthApi
import eu.vardalabs.organizer.api.apis.DrivesApi
import eu.vardalabs.organizer.api.apis.DropBoxApi
import eu.vardalabs.organizer.api.apis.EncryptionApi
import eu.vardalabs.organizer.api.apis.FilesApi
import eu.vardalabs.organizer.api.apis.NotificationsApi
import eu.vardalabs.organizer.api.apis.SharingApi
import eu.vardalabs.organizer.core.session.Session
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Response

/**
 * The API of one signed-in session: an OkHttp client that sends the device
 * token as `Authorization: Bearer` and calls [onUnauthorized] on a 401 (the
 * token was revoked: the app signs out), and the generated clients on it.
 */
class Api(
    val session: Session,
    base: OkHttpClient,
    onUnauthorized: () -> Unit,
) {
    init {
        GeneratedJson.ensure()
    }

    val server: HttpUrl = session.server.toHttpUrl()

    val http: OkHttpClient =
        base
            .newBuilder()
            .addInterceptor(BearerInterceptor(session.token, onUnauthorized))
            .build()

    val files = FilesApi(session.server, http)
    val dropBox = DropBoxApi(session.server, http)
    val auth = AuthApi(session.server, http)
    val notifications = NotificationsApi(session.server, http)
    val encryption = EncryptionApi(session.server, http)
    val drives = DrivesApi(session.server, http)
    val sharing = SharingApi(session.server, http)

    /** A path of the API on this server, e.g. `api/files/{id}/content`. */
    fun url(path: String): HttpUrl = server.newBuilder().addPathSegments(path).build()
}

private class BearerInterceptor(
    private val token: String,
    private val onUnauthorized: () -> Unit,
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val response =
            chain.proceed(
                chain
                    .request()
                    .newBuilder()
                    .header("Authorization", "Bearer $token")
                    .build(),
            )
        if (response.code == 401) onUnauthorized()
        return response
    }
}
