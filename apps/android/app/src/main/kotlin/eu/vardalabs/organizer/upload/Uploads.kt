package eu.vardalabs.organizer.upload

import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkInfo
import androidx.work.WorkManager
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import java.util.UUID
import java.util.concurrent.TimeUnit

/** An upload as the screens show it. */
data class UploadInfo(
    val id: UUID,
    val name: String,
    /** The folder it goes to, or null for the drop box. */
    val folderId: String?,
    val state: State,
    val sent: Long,
    val total: Long,
    val error: String?,
) {
    enum class State { WAITING, RUNNING, FAILED, DONE }

    /** How far along a running upload is, from 0 to 1; null when not known. */
    val fraction: Float?
        get() = if (total <= 0 || state != State.RUNNING) null else (sent.toFloat() / total).coerceIn(0f, 1f)
}

/** The uploads, as WorkManager jobs: they go on when the app is closed, and retry. */
class Uploads(
    private val work: WorkManager,
) {
    fun enqueue(request: UploadRequest): UUID {
        val job =
            OneTimeWorkRequestBuilder<UploadWorker>()
                .setInputData(request.toData())
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
                .addTag(TAG)
                .addTag(NAME_TAG + request.name)
                .addTag(request.folderId?.let { FOLDER_TAG + it } ?: DROP_BOX_TAG)
                .build()
        work.enqueue(job)
        return job.id
    }

    /** Every upload WorkManager still knows, the newest last. */
    val all: Flow<List<UploadInfo>> =
        work.getWorkInfosByTagFlow(TAG).map { infos -> infos.map(::info) }

    /** Signed out: what waits is dropped. */
    fun cancelAll() {
        work.cancelAllWorkByTag(TAG)
    }

    /** Forget finished uploads, so that old failures are not shown again. */
    fun prune() {
        work.pruneWork()
    }

    private fun info(work: WorkInfo): UploadInfo {
        val name =
            work.tags
                .firstOrNull { it.startsWith(NAME_TAG) }
                ?.removePrefix(NAME_TAG)
                .orEmpty()
        val folder = work.tags.firstOrNull { it.startsWith(FOLDER_TAG) }?.removePrefix(FOLDER_TAG)
        val state =
            when (work.state) {
                WorkInfo.State.RUNNING -> UploadInfo.State.RUNNING
                WorkInfo.State.SUCCEEDED -> UploadInfo.State.DONE
                WorkInfo.State.FAILED, WorkInfo.State.CANCELLED -> UploadInfo.State.FAILED
                else -> UploadInfo.State.WAITING
            }
        return UploadInfo(
            id = work.id,
            name = name,
            folderId = folder,
            state = state,
            sent = work.progress.getLong(UploadWorker.SENT, 0),
            total = work.progress.getLong(UploadWorker.TOTAL, 0),
            error =
                work.outputData.getString(UploadWorker.ERROR)
                    ?: if (work.state == WorkInfo.State.CANCELLED) "Cancelled" else null,
        )
    }

    companion object {
        const val TAG = "upload"
        private const val NAME_TAG = "name:"
        private const val FOLDER_TAG = "folder:"
        private const val DROP_BOX_TAG = "drop-box"
    }
}
