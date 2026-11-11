package eu.vardalabs.organizer.upload

import android.content.Context
import androidx.core.content.edit
import eu.vardalabs.organizer.core.upload.ResumeStore

/**
 * Where each resumable upload is on the server, by its work id, so that a
 * retry after the app was killed goes on from the server's offset. Only
 * upload URLs: no token, no content.
 */
class PrefsResumeStore(
    context: Context,
) : ResumeStore {
    private val prefs = context.getSharedPreferences("uploads", Context.MODE_PRIVATE)

    override fun get(key: String): String? = prefs.getString(key, null)

    override fun put(
        key: String,
        value: String,
    ) = prefs.edit(commit = true) { putString(key, value) }

    override fun remove(key: String) = prefs.edit(commit = true) { remove(key) }
}
