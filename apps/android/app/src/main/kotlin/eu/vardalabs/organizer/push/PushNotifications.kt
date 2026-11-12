package eu.vardalabs.organizer.push

import eu.vardalabs.organizer.api.models.Notice
import eu.vardalabs.organizer.core.api.ApiException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.io.IOException

/** The server's side of notifications, for the signed-in user (`/api/notifications`). */
interface PushServer {
    /** The server's VAPID public key, base64url: what the distributor registers with. */
    suspend fun vapidKey(): String

    /** Send this device's endpoint and keys (kind `unifiedpush`); its subscription's id. */
    suspend fun subscribe(
        name: String,
        endpoint: String,
        p256dh: String,
        auth: String,
    ): String

    suspend fun unsubscribe(id: String)

    /** What is new, newest first. */
    suspend fun news(): List<Notice>
}

/** The user's UnifiedPush distributor (ntfy, NextPush, ...), as the connector reaches it. */
interface Distributor {
    /** The name of the distributor in use, or null. */
    fun current(): String?

    /**
     * The distributor in use, or else the user's default one when it needs no
     * choice (it is then saved); null when there is none or the user must pick.
     */
    fun resolve(): String?

    fun register(vapid: String)

    fun unregister()
}

/** What is kept on the device between pushes. */
interface PushPrefs {
    /** Notifications are on for this device (the default). */
    var enabled: Boolean

    /** The server's id of this device's subscription. */
    var subscriptionId: String?

    /** The last notice shown, not to show it twice. */
    var lastNotice: String?
}

/** The switch in Settings: on with a distributor, or off; [missing] after a pick that found none. */
data class PushStatus(
    val on: Boolean,
    val distributor: String? = null,
    val missing: Boolean = false,
)

/** What Settings needs of the notifications. */
interface Notifications {
    val status: StateFlow<PushStatus>

    /** Turned on, once the screen let the user pick a distributor ([picked] false: none). */
    suspend fun enable(picked: Boolean)

    suspend fun disable()

    /** Before the token is revoked: no more pushes here, the subscription gone from the server. */
    suspend fun signOut()
}

/**
 * Push notifications through UnifiedPush (docs/M6D_DESIGN.md): the
 * distributor gives an endpoint and Web Push keys, which go to the server as
 * a subscription. A push carries no content: the app then asks the server
 * what is new and shows the newest notice.
 */
class PushNotifications(
    /** The signed-in user's server, or null when signed out. */
    private val server: suspend () -> PushServer?,
    private val distributor: Distributor,
    private val prefs: PushPrefs,
    /** What the server's list of devices shows. */
    private val deviceName: String,
) : Notifications {
    private val state =
        MutableStateFlow(PushStatus(on = prefs.enabled && distributor.current() != null, distributor.current()))
    override val status: StateFlow<PushStatus> = state.asStateFlow()

    /**
     * After sign-in and at each start: register with the distributor (again,
     * as the connector recommends: it checks the distributor is still there,
     * and a new endpoint comes to [onNewEndpoint]). The user is never asked
     * here: with no distributor, or several and no default, nothing happens.
     */
    suspend fun start() {
        if (!prefs.enabled) return refresh()
        val api = server() ?: return
        if (distributor.resolve() == null) return refresh()
        distributor.register(api.vapidKey())
        refresh()
    }

    override suspend fun enable(picked: Boolean) {
        if (!picked) {
            state.value = PushStatus(on = false, missing = true)
            return
        }
        prefs.enabled = true
        start()
    }

    override suspend fun disable() {
        prefs.enabled = false
        stop()
    }

    override suspend fun signOut() {
        stop()
        prefs.lastNotice = null
    }

    /** No more pushes here; the server forgets this device when it can be reached. */
    private suspend fun stop() {
        distributor.unregister()
        try {
            removeSubscription()
        } catch (_: IOException) {
            // Offline: the row stays on the server (removable on the web, under
            // Settings → Notifications) or goes when its push service says the endpoint is gone.
            prefs.subscriptionId = null
        }
        refresh()
    }

    /** Signed out without the server (the token was revoked): only this device forgets. */
    fun forget() {
        distributor.unregister()
        prefs.subscriptionId = null
        prefs.lastNotice = null
        refresh()
    }

    /** The distributor gave an endpoint: it goes to the server (the same endpoint replaces its row). */
    suspend fun onNewEndpoint(
        endpoint: String,
        p256dh: String,
        auth: String,
    ) {
        val api = server() ?: return
        val id = api.subscribe(deviceName.take(100), endpoint, p256dh, auth)
        val old = prefs.subscriptionId
        prefs.subscriptionId = id
        if (old != null && old != id) {
            try {
                api.unsubscribe(old)
            } catch (_: IOException) {
                // Gone already, or offline: its push service ends it.
            }
        }
        refresh()
    }

    /** The distributor ended the registration: the server forgets this device. */
    suspend fun onUnregistered() {
        removeSubscription()
        refresh()
    }

    /**
     * A push came: the newest notice, to show, or null when there is none (a
     * wake-up for something already seen, or signed out). A notice shown
     * already is not shown again.
     */
    suspend fun onMessage(): Notice? {
        val api = server() ?: return null
        val newest = api.news().firstOrNull() ?: return null
        val id = newest.id.toString()
        if (id == prefs.lastNotice) return null
        prefs.lastNotice = id
        return newest
    }

    private suspend fun removeSubscription() {
        val id = prefs.subscriptionId ?: return
        val api = server()
        if (api != null) {
            try {
                api.unsubscribe(id)
            } catch (e: ApiException) {
                if (e.status != 404) throw e
            }
        }
        prefs.subscriptionId = null
    }

    private fun refresh() {
        val name = distributor.current()
        state.value = PushStatus(on = prefs.enabled && name != null, distributor = name)
    }
}
