package eu.vardalabs.organizer.core.api

import eu.vardalabs.organizer.api.infrastructure.ClientError
import eu.vardalabs.organizer.api.infrastructure.ClientException
import eu.vardalabs.organizer.api.infrastructure.ServerError
import eu.vardalabs.organizer.api.infrastructure.ServerException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Response
import java.io.IOException

/**
 * An answer of the server that is not a success, with its `{"error": "..."}`
 * message, and its `code` when it has one (what a client acts on).
 */
open class ApiException(
    val status: Int,
    message: String,
    val code: String? = null,
) : IOException(message) {
    /**
     * The drive's storage cannot be used now (docs/M7C_DESIGN.md §4): its
     * backend fails, or its owner's private credentials are locked.
     */
    val storageUnavailable: Boolean get() = status == 503 && code == STORAGE_UNAVAILABLE

    /**
     * The drive is being moved to other storage: read-only until done, never
     * a version conflict.
     */
    val readOnly: Boolean get() = status == 409 && code == READ_ONLY

    /** The other server of a share from another server cannot be reached now. */
    val remoteUnreachable: Boolean get() = code == REMOTE_UNREACHABLE

    /**
     * Worth trying again later: the server or the network (the other server
     * of a share too, unless it refused), or a drive being moved; not the request.
     */
    val retryable: Boolean
        get() = (status >= 500 && code != REMOTE_REFUSED) || status == 408 || status == 429 || readOnly

    companion object {
        /** The `code` of a 503 whose drive's storage cannot be used now. */
        const val STORAGE_UNAVAILABLE = "storage_unavailable"

        /** The `code` of a 409 refusing a write to a drive being moved to other storage. */
        const val READ_ONLY = "read_only"

        /** The other server of a share (docs/M7E_DESIGN.md §10) cannot be reached now (a 502): worth a retry. */
        const val REMOTE_UNREACHABLE = "remote_unreachable"

        /** The other server of a share refused the request (a 502). */
        const val REMOTE_REFUSED = "remote_refused"

        /** Federation is off on this server. */
        const val FEDERATION_OFF = "federation_off"

        /** This server refuses the other one. */
        const val SERVER_REFUSED = "server_refused"

        /**
         * What the app says for a federation refusal, by its code (the rest of
         * them, `contact_required`, `federation_forbidden`, `e2e_unsupported`,
         * `drives_unsupported`, `invite_invalid`, `invite_used`, come only from
         * sharing and invites, which are on the web: their server's words).
         */
        private fun federationMessage(code: String?): String? =
            when (code) {
                REMOTE_UNREACHABLE -> "The other server cannot be reached now. Try again later."
                REMOTE_REFUSED -> "The other server refused this."
                FEDERATION_OFF -> "Sharing with other servers is off on this server."
                SERVER_REFUSED -> "This server no longer accepts the other one."
                else -> null
            }

        /**
         * The exception of an answer's [status] and error [body] (null or not
         * JSON: its status). A drive's storage unavailable says so, with the
         * server's reason ("Locked until … signs in"); a refusal about another
         * server (docs/M7E_DESIGN.md) says what it is, by its code.
         */
        fun of(
            status: Int,
            body: String?,
        ): ApiException {
            val error = errorBody(body)
            val message = error?.first ?: "The server answered $status"
            val code = error?.second
            return if (status == 503 && code == STORAGE_UNAVAILABLE) {
                ApiException(status, "This drive is not available now: $message.", code)
            } else {
                ApiException(status, federationMessage(code) ?: message, code)
            }
        }
    }
}

private val lenient = Json { ignoreUnknownKeys = true }

/** The message and code of an error body (`{"error": "...", "code": "..."}`), or null. */
internal fun errorBody(body: String?): Pair<String, String?>? =
    body?.let {
        runCatching {
            val json = lenient.parseToJsonElement(it).jsonObject
            json["error"]?.jsonPrimitive?.content?.let { message ->
                message to json["code"]?.jsonPrimitive?.takeIf { code -> code.isString }?.content
            }
        }.getOrNull()
    }

/** An [ApiException] for a response that is not a success (its body is read). */
fun Response.toApiException(): ApiException = ApiException.of(code, runCatching { body.string() }.getOrNull())

/**
 * Run a call of the generated client off the main thread (interruptible, so
 * a cancelled coroutine cancels the request), its errors made [ApiException].
 */
suspend fun <T> apiCall(block: () -> T): T =
    runInterruptible(Dispatchers.IO) {
        try {
            block()
        } catch (e: ClientException) {
            val body = (e.response as? ClientError<*>)?.body?.toString()
            throw ApiException.of(e.statusCode, body)
        } catch (e: ServerException) {
            val body = (e.response as? ServerError<*>)?.body?.toString()
            throw ApiException.of(e.statusCode, body)
        }
    }
