package eu.vardalabs.organizer.core.events

import eu.vardalabs.organizer.core.api.Api
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.retryWhen
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources
import java.io.IOException
import java.util.concurrent.TimeUnit

/** What `/api/events` tells the open app. */
enum class LiveEvent {
    /** (Re)connected: what is on screen may be stale. */
    Connected,

    /** The change feed has new rows: the folder on screen may have changed. */
    Changes,

    /** An item of the drop box changed. */
    DropBox,
}

/**
 * The live events (server-sent events) while collected, reconnecting with
 * a growing pause when the connection drops. The server sends a comment
 * every 30 seconds, so a read that waits longer means a dead connection.
 */
fun liveEvents(api: Api): Flow<LiveEvent> = liveEvents(api.http, api.url("api/events").toString())

internal fun liveEvents(
    http: OkHttpClient,
    url: String,
): Flow<LiveEvent> {
    val client = http.newBuilder().readTimeout(75, TimeUnit.SECONDS).build()
    return callbackFlow {
        val request =
            Request
                .Builder()
                .url(url)
                .header("Accept", "text/event-stream")
                .build()
        val source =
            EventSources.createFactory(client).newEventSource(
                request,
                object : EventSourceListener() {
                    override fun onOpen(
                        eventSource: EventSource,
                        response: Response,
                    ) {
                        trySend(LiveEvent.Connected)
                    }

                    override fun onEvent(
                        eventSource: EventSource,
                        id: String?,
                        type: String?,
                        data: String,
                    ) {
                        when (type) {
                            "changes" -> trySend(LiveEvent.Changes)
                            "drop-box" -> trySend(LiveEvent.DropBox)
                        }
                    }

                    override fun onClosed(eventSource: EventSource) {
                        close(IOException("The event stream closed"))
                    }

                    override fun onFailure(
                        eventSource: EventSource,
                        t: Throwable?,
                        response: Response?,
                    ) {
                        close(t ?: IOException("The event stream failed (${response?.code})"))
                    }
                },
            )
        awaitClose { source.cancel() }
    }.retryWhen { _, attempt ->
        delay(minOf(30_000L, 1_000L * (1L shl attempt.toInt().coerceAtMost(5))))
        true
    }
}
