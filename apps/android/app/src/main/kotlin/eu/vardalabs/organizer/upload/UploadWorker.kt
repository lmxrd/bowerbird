package eu.vardalabs.organizer.upload

import android.app.Notification
import android.content.Context
import android.content.pm.ServiceInfo
import androidx.core.app.NotificationCompat
import androidx.work.CoroutineWorker
import androidx.work.ForegroundInfo
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import eu.vardalabs.organizer.OrganizerApplication
import eu.vardalabs.organizer.R
import eu.vardalabs.organizer.api.models.DropLink
import eu.vardalabs.organizer.api.models.Links
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.api.apiCall
import eu.vardalabs.organizer.core.upload.DropOptions
import eu.vardalabs.organizer.core.upload.UploadClient
import eu.vardalabs.organizer.core.upload.UploadProgress
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible
import java.io.IOException
import java.util.UUID

/**
 * One upload (docs/M4_DESIGN.md §2): a file into a folder or the drop box
 * (resumable from [UploadClient.PLAIN_LIMIT], where the upload's URL is kept
 * between tries), or a link into the drop box. A large file runs as a foreground
 * service with its progress in a notification, so that it goes on when the
 * app is closed.
 */
class UploadWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    private val container get() = (applicationContext as OrganizerApplication).container
    private val request by lazy { UploadRequest.from(inputData) }
    private val notificationId get() = id.hashCode()

    override suspend fun getForegroundInfo(): ForegroundInfo = foregroundInfo(0, request.size)

    override suspend fun doWork(): Result {
        val session = container.sessions.current() ?: return fail("Signed out")
        val api = container.apiFor(session)
        val client = UploadClient(api.http, api.server)
        if (request.size >= UploadClient.PLAIN_LIMIT) {
            // Not allowed from the background on some versions: the upload goes on without it.
            runCatching { setForeground(foregroundInfo(0, request.size)) }
        }
        return try {
            when (request.kind) {
                UploadKind.LINK -> {
                    val link =
                        DropLink(
                            url = request.url ?: request.name,
                            mode = request.linkMode?.let { mode -> Links.entries.firstOrNull { it.value == mode } },
                            note = request.note?.ifBlank { null },
                            readContents = request.readContents,
                        )
                    apiCall { api.dropBox.dropLink(link) }
                }

                UploadKind.DROP_BOX -> {
                    val source = staging.source(request)
                    val options = DropOptions(request.note, request.readContents)
                    // The work's id keys the upload's URL, as below.
                    runInterruptible(Dispatchers.IO) {
                        client.drop(source, options, container.resumeStore, id.toString(), progress())
                    }
                }

                UploadKind.FOLDER -> {
                    val source = staging.source(request)
                    val folder = UUID.fromString(requireNotNull(request.folderId))
                    val sealed = request.encryptedName
                    // The work's id keys the upload's URL: a retry goes on where the server is.
                    runInterruptible(Dispatchers.IO) {
                        when {
                            // Encrypted: the name as sealed (the device checked it is free), and no
                            // numbering on a clash, which the server cannot see.
                            sealed == null -> {
                                client.upload(folder, source, container.resumeStore, id.toString(), progress())
                            }

                            source.length < UploadClient.PLAIN_LIMIT -> {
                                client.uploadPlain(folder, source, sealed, progress())
                            }

                            else -> {
                                client.uploadResumable(
                                    folder,
                                    source,
                                    sealed,
                                    container.resumeStore,
                                    id.toString(),
                                    progress(),
                                )
                            }
                        }
                    }
                }
            }
            staging.release(request)
            Result.success()
        } catch (e: ApiException) {
            when {
                e.status == 401 -> fail("Signed out")
                e.retryable && runAttemptCount < MAX_ATTEMPTS -> Result.retry()
                else -> fail(e.message ?: "The upload failed")
            }
        } catch (e: IOException) {
            if (runAttemptCount < MAX_ATTEMPTS) Result.retry() else fail(e.message ?: "The upload failed")
        } catch (e: IllegalArgumentException) {
            fail(e.message ?: "The file cannot be read")
        } catch (e: SecurityException) {
            fail("Organizer may no longer read this file")
        }
    }

    /** A failure for good: the copy made for it goes too. */
    private fun fail(message: String): Result {
        runCatching { staging.release(request) }
        return Result.failure(workDataOf(ERROR to message))
    }

    private val staging get() = container.staging

    /** Progress for WorkManager and the notification, at most a few times a second. */
    private fun progress(): UploadProgress {
        var last = 0L
        return UploadProgress { sent, total ->
            val now = System.currentTimeMillis()
            if (now - last >= 500 || sent == total) {
                last = now
                setProgressAsync(workDataOf(SENT to sent, TOTAL to total))
                if (total >= UploadClient.PLAIN_LIMIT) runCatching { setForegroundAsync(foregroundInfo(sent, total)) }
            }
        }
    }

    private fun foregroundInfo(
        sent: Long,
        total: Long,
    ): ForegroundInfo {
        val percent = if (total > 0) (sent * 100 / total).toInt() else 0
        val notification: Notification =
            NotificationCompat
                .Builder(applicationContext, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_upload)
                .setContentTitle(applicationContext.getString(R.string.uploading, request.name))
                .setProgress(100, percent, total <= 0)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setSilent(true)
                .build()
        return ForegroundInfo(notificationId, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
    }

    companion object {
        const val CHANNEL = "uploads"
        const val SENT = "sent"
        const val TOTAL = "total"
        const val ERROR = "error"
        private const val MAX_ATTEMPTS = 10
    }
}
