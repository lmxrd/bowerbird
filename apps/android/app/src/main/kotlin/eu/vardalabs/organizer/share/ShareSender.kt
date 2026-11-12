package eu.vardalabs.organizer.share

import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.upload.Prepared
import eu.vardalabs.organizer.upload.Staging
import eu.vardalabs.organizer.upload.UploadKind
import eu.vardalabs.organizer.upload.UploadRequest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.net.URI
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter

/**
 * Turns what was shared into uploads: each file made readable for later
 * (kept access, or a copy), each link and text as the server takes it, then
 * all handed to [enqueue]. The uploads themselves run afterwards, in the
 * worker. Into an encrypted folder, everything is a file, encrypted here
 * first ([encrypted]).
 */
class ShareSender(
    private val staging: Staging,
    private val encrypted: suspend (Folder, List<Prepared>) -> List<UploadRequest>,
    private val enqueue: (UploadRequest) -> Unit,
) {
    suspend fun send(
        items: List<SharedItem>,
        choice: ShareChoice,
    ) = withContext(Dispatchers.IO) {
        val folder = choice.folder?.takeIf { choice.mode == ShareMode.FOLDER && it.e2eRoot != null }
        if (folder != null) {
            val prepared =
                items.map { item ->
                    when (item) {
                        is SharedItem.File -> {
                            staging.prepare(item.uri)
                        }

                        is SharedItem.Link -> {
                            staging.prepareText(
                                linkFileName(item.url, item.title),
                                shortcut(item.url),
                            )
                        }

                        is SharedItem.Text -> {
                            staging.prepareText(
                                textFileName(item.title, LocalDateTime.now()),
                                item.text,
                            )
                        }
                    }
                }
            encrypted(folder, prepared).forEach(enqueue)
            return@withContext
        }
        val requests =
            items.map { item ->
                when (item) {
                    is SharedItem.File -> {
                        request(staging.prepare(item.uri), choice)
                    }

                    is SharedItem.Link -> {
                        if (choice.mode == ShareMode.DROP_BOX) {
                            UploadRequest(
                                kind = UploadKind.LINK,
                                name = item.url,
                                url = item.url,
                                note = choice.note,
                                readContents = choice.readContents,
                                linkMode = choice.linkMode?.value,
                            )
                        } else {
                            request(staging.prepareText(linkFileName(item.url, item.title), shortcut(item.url)), choice)
                        }
                    }

                    is SharedItem.Text -> {
                        request(staging.prepareText(textFileName(item.title, LocalDateTime.now()), item.text), choice)
                    }
                }
            }
        requests.forEach(enqueue)
    }

    private fun request(
        prepared: Prepared,
        choice: ShareChoice,
    ) = UploadRequest(
        kind = if (choice.mode == ShareMode.DROP_BOX) UploadKind.DROP_BOX else UploadKind.FOLDER,
        name = prepared.name,
        source = prepared.source,
        size = prepared.size,
        staged = prepared.staged,
        folderId = folderId(choice),
        note = choice.note,
        readContents = choice.readContents,
    )

    private fun folderId(choice: ShareChoice): String? {
        if (choice.mode != ShareMode.FOLDER) return null
        return requireNotNull(choice.folder) { "Choose a folder" }.id.toString()
    }

    companion object {
        /** A link placed by hand: an internet shortcut, as browsers and file managers open it. */
        fun shortcut(url: String) = "[InternetShortcut]\r\nURL=$url\r\n"

        /** `Page title.url`, or the link's host: `example.com.url`. */
        fun linkFileName(
            url: String,
            title: String?,
        ): String {
            val base = title?.let(::clean) ?: runCatching { URI(url).host }.getOrNull()?.let(::clean) ?: "Link"
            return "$base.url"
        }

        /** `Title.txt`, or `Note 2026-11-07 1432.txt`. */
        fun textFileName(
            title: String?,
            now: LocalDateTime,
        ): String {
            val base = title?.let(::clean) ?: "Note ${now.format(DateTimeFormatter.ofPattern("yyyy-MM-dd HHmm"))}"
            return "$base.txt"
        }

        private fun clean(raw: String): String? =
            raw
                .replace(Regex("""[\\/:*?"<>|\u0000-\u001f]"""), " ")
                .replace(Regex("\\s+"), " ")
                .trim()
                .trim('.')
                .take(100)
                .ifEmpty { null }
    }
}
