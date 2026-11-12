package eu.vardalabs.organizer.push

import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.util.Log
import androidx.core.content.edit
import eu.vardalabs.organizer.api.models.DeviceKind
import eu.vardalabs.organizer.api.models.NewSubscription
import eu.vardalabs.organizer.api.models.Notice
import eu.vardalabs.organizer.api.models.SubscriptionKeys
import eu.vardalabs.organizer.core.api.Api
import eu.vardalabs.organizer.core.api.apiCall
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.ResolvedDistributor
import java.util.UUID

/** The distributor through UnifiedPush's connector (its default instance). */
class UnifiedPushDistributor(
    private val context: Context,
) : Distributor {
    override fun current(): String? =
        (
            UnifiedPush.getAckDistributor(context)
                ?: UnifiedPush.getSavedDistributor(context)
        )?.let(::label)

    override fun resolve(): String? {
        UnifiedPush.getAckDistributor(context)?.let { return it }
        UnifiedPush.getSavedDistributor(context)?.let { return it }
        val found = UnifiedPush.resolveDefaultDistributor(context) as? ResolvedDistributor.Found ?: return null
        UnifiedPush.saveDistributor(context, found.packageName)
        return found.packageName
    }

    override fun register(vapid: String) {
        try {
            UnifiedPush.register(context, messageForDistributor = "Organizer", vapid = vapid)
        } catch (_: UnifiedPush.VapidNotValidException) {
            // The server's key is not an uncompressed P-256 point: nothing to register with.
            Log.w("OrganizerPush", "The server's VAPID key is not valid")
        }
    }

    override fun unregister() = UnifiedPush.unregister(context)

    private fun label(packageName: String): String =
        try {
            val info = context.packageManager.getApplicationInfo(packageName, 0)
            context.packageManager.getApplicationLabel(info).toString()
        } catch (_: PackageManager.NameNotFoundException) {
            packageName
        }
}

/**
 * The user's distributor, or the system's screen to pick one when there are
 * several and no default (so only on the user's tap); [done] says whether
 * there is one now.
 */
fun pickDistributor(
    activity: Activity,
    done: (Boolean) -> Unit,
) = UnifiedPush.tryUseCurrentOrDefaultDistributor(activity, done)

/** Only ids and a switch: no token, no content. */
class SharedPrefsPushPrefs(
    context: Context,
) : PushPrefs {
    private val prefs = context.getSharedPreferences("push", Context.MODE_PRIVATE)

    override var enabled: Boolean
        get() = prefs.getBoolean(ENABLED, true)
        set(value) = prefs.edit(commit = true) { putBoolean(ENABLED, value) }

    override var subscriptionId: String?
        get() = prefs.getString(SUBSCRIPTION, null)
        set(value) = prefs.edit(commit = true) { putString(SUBSCRIPTION, value) }

    override var lastNotice: String?
        get() = prefs.getString(LAST_NOTICE, null)
        set(value) = prefs.edit(commit = true) { putString(LAST_NOTICE, value) }

    private companion object {
        const val ENABLED = "enabled"
        const val SUBSCRIPTION = "subscription_id"
        const val LAST_NOTICE = "last_notice"
    }
}

/** [PushServer] on the signed-in session's API. */
class ApiPushServer(
    private val api: Api,
) : PushServer {
    override suspend fun vapidKey(): String = apiCall { api.notifications.getNotifications() }.vapidPublicKey

    override suspend fun subscribe(
        name: String,
        endpoint: String,
        p256dh: String,
        auth: String,
    ): String {
        val subscription =
            NewSubscription(
                endpoint = endpoint,
                kind = DeviceKind.UNIFIEDPUSH,
                name = name,
                propertyKeys = SubscriptionKeys(auth = auth, p256dh = p256dh),
            )
        return apiCall { api.notifications.addPushSubscription(subscription) }.id.toString()
    }

    override suspend fun unsubscribe(id: String) {
        apiCall { api.notifications.removePushSubscription(UUID.fromString(id)) }
    }

    override suspend fun news(): List<Notice> = apiCall { api.notifications.getNews() }.notices
}
