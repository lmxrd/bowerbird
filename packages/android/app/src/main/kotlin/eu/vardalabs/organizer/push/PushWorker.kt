package eu.vardalabs.organizer.push

import android.Manifest
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager.PERMISSION_GRANTED
import android.content.pm.ServiceInfo
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import eu.vardalabs.organizer.MainActivity
import eu.vardalabs.organizer.OrganizerApplication
import eu.vardalabs.organizer.R
import eu.vardalabs.organizer.api.models.Notice
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.upload.UploadWorker
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import java.io.IOException
import java.util.concurrent.TimeUnit

/**
 * UnifiedPush's events, from the user's distributor. The service lives only
 * a few seconds, so what needs the network is a [PushWorker].
 */
class OrganizerPushService : PushService() {
    override fun onNewEndpoint(
        endpoint: PushEndpoint,
        instance: String,
    ) {
        val keys = endpoint.pubKeySet
        if (keys == null) {
            Log.w(TAG, "An endpoint without Web Push keys")
            return
        }
        val data =
            workDataOf(
                PushWorker.URL to endpoint.url,
                PushWorker.P256DH to keys.pubKey,
                PushWorker.AUTH to keys.auth,
            )
        PushWorker.enqueue(this, PushWorker.ENDPOINT, data)
    }

    override fun onMessage(
        message: PushMessage,
        instance: String,
    ) {
        // The content is always "{}": the push only says something is new.
        PushWorker.enqueue(this, PushWorker.MESSAGE)
    }

    override fun onUnregistered(instance: String) = PushWorker.enqueue(this, PushWorker.UNREGISTERED)

    override fun onRegistrationFailed(
        reason: FailedReason,
        instance: String,
    ) {
        Log.w(TAG, "Registration failed: $reason")
    }

    private companion object {
        const val TAG = "OrganizerPush"
    }
}

/** One push event that needs the server: an endpoint to send, a registration ended, news to show. */
class PushWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    private val push get() = (applicationContext as OrganizerApplication).container.push

    // Expedited work runs as a foreground service before Android 12: it needs a notification.
    override suspend fun getForegroundInfo(): ForegroundInfo {
        val notification =
            NotificationCompat
                .Builder(applicationContext, UploadWorker.CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_notice)
                .setContentTitle(applicationContext.getString(R.string.app_name))
                .setSilent(true)
                .build()
        return ForegroundInfo(FOREGROUND_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
    }

    override suspend fun doWork(): Result =
        try {
            when (tags.firstOrNull { it in EVENTS }) {
                ENDPOINT -> {
                    push.onNewEndpoint(
                        requireNotNull(inputData.getString(URL)),
                        requireNotNull(inputData.getString(P256DH)),
                        requireNotNull(inputData.getString(AUTH)),
                    )
                }

                UNREGISTERED -> {
                    push.onUnregistered()
                }

                MESSAGE -> {
                    push.onMessage()?.let { show(applicationContext, it) }
                }
            }
            Result.success()
        } catch (e: ApiException) {
            if (e.retryable && runAttemptCount < MAX_ATTEMPTS) Result.retry() else Result.failure()
        } catch (_: IOException) {
            if (runAttemptCount < MAX_ATTEMPTS) Result.retry() else Result.failure()
        }

    companion object {
        const val ENDPOINT = "push-endpoint"
        const val UNREGISTERED = "push-unregistered"
        const val MESSAGE = "push-message"
        const val URL = "url"
        const val P256DH = "p256dh"
        const val AUTH = "auth"
        private val EVENTS = setOf(ENDPOINT, UNREGISTERED, MESSAGE)
        private const val MAX_ATTEMPTS = 5
        private const val FOREGROUND_ID = 0x5055

        /** The newest event of a kind replaces one that waits: only the latest endpoint, one fetch of the news. */
        fun enqueue(
            context: Context,
            event: String,
            data: Data = workDataOf(),
        ) {
            val job =
                OneTimeWorkRequestBuilder<PushWorker>()
                    .setInputData(data)
                    .addTag(event)
                    .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                    .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                    .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
                    .build()
            WorkManager.getInstance(context).enqueueUniqueWork(event, ExistingWorkPolicy.REPLACE, job)
        }
    }
}

/** The channel of the notices ("Notifications"), made at start. */
const val NOTICES_CHANNEL = "notices"

/** The extra of [MainActivity]'s intent with the page a notice opens. */
const val EXTRA_PATH = "eu.vardalabs.organizer.PATH"

/** A notice as a system notification; a tap opens the app at its page. One per kind: a newer count replaces the last. */
fun show(
    context: Context,
    notice: Notice,
) {
    if (Build.VERSION.SDK_INT >= 33 &&
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PERMISSION_GRANTED
    ) {
        return
    }
    val open =
        Intent(context, MainActivity::class.java)
            .setAction(Intent.ACTION_VIEW)
            .putExtra(EXTRA_PATH, notice.path)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
    val tap =
        PendingIntent.getActivity(
            context,
            notice.kind.value.hashCode(),
            open,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    val notification =
        NotificationCompat
            .Builder(context, NOTICES_CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_notice)
            .setContentTitle(notice.title)
            .setContentText(notice.body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(notice.body))
            .setContentIntent(tap)
            .setAutoCancel(true)
            .build()
    NotificationManagerCompat.from(context).notify(notice.kind.value, 0, notification)
}
